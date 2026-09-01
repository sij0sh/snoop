use std::path::Path;

use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("README.md"),
        "# Session policy\n\nRefresh tokens are validated before a session is rotated.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        r#"
pub struct Token;

pub fn refresh_session(token: Token) {
    validate_token(token);
    retry();
    retry();
    retry();
}

pub fn process() {
    let note = "authentication token session authentication token session authentication token session";
    println!("{}", note);
}

fn validate_token(_token: Token) {}
fn retry() {}
"#,
    )
    .unwrap();
}

#[test]
fn indexes_incrementally_and_queries_four_channels() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");

    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(first.changed_sources, 2);
    assert_eq!(first.unchanged_sources, 0);
    assert!(first.units_added >= 4);
    assert_eq!(first.embedded, store.stats().unwrap().units as usize * 2);

    let unit_ids = store.unit_ids().unwrap();
    let readme_id = store.source_by_locator("README.md").unwrap().unwrap().id;
    let second =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(second.changed_sources, 0);
    assert_eq!(second.unchanged_sources, 2);
    assert_eq!(second.embedded, 0);
    assert_eq!(store.unit_ids().unwrap(), unit_ids);
    assert_eq!(
        store.source_by_locator("README.md").unwrap().unwrap().id,
        readme_id
    );

    let dual = query(
        &store,
        Some(&embedder),
        "authentication token session",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 2_000,
            diagnostics: true,
            ..QueryOptions::default()
        },
    )
    .unwrap();
    let dual_debug = dual.debug.as_ref().unwrap();
    assert!(!dual_debug.evidence_lexical.is_empty());
    assert!(!dual_debug.evidence_vector.is_empty());
    assert!(!dual_debug.routing_lexical.is_empty());
    assert!(!dual_debug.routing_vector.is_empty());
    assert!(dual.packet.token_count <= 2_000);
    assert!(dual
        .debug
        .as_ref()
        .unwrap()
        .items
        .iter()
        .all(|item| !item.source_slices.is_empty()));
    assert!(dual
        .packet
        .items
        .iter()
        .any(|item| item.evidence_text.contains("refresh_session")));
    let repeated = query(
        &store,
        Some(&embedder),
        "authentication token session",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 10,
            max_tokens: 2_000,
            diagnostics: false,
            ..QueryOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::to_string(&dual.packet).unwrap(),
        serde_json::to_string(&repeated.packet).unwrap()
    );

    let before = store.stats().unwrap();
    std::fs::write(
        directory.path().join("README.md"),
        "# Session policy\n\nRefresh tokens are validated before rotation.\n\n## Order\n\nValidation is first.\n",
    )
    .unwrap();
    let third =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(third.changed_sources, 1);
    assert_eq!(third.unchanged_sources, 1);
    assert!(third.embedded > 0);
    assert_eq!(store.stats().unwrap().sources, before.sources);

    std::fs::remove_file(directory.path().join("README.md")).unwrap();
    let fourth =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(fourth.deleted_sources, 1);
    assert_eq!(store.stats().unwrap().sources, before.sources - 1);
}

#[test]
fn index_format_version_forces_a_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let first =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    store.set_repository_content_version("obsolete").unwrap();
    let rebuilt =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    assert_eq!(rebuilt.changed_sources, 2);
    assert_eq!(rebuilt.unchanged_sources, 0);
}

#[test]
fn deterministic_routing_changes_the_top_one_on_a_vocabulary_fixture() {
    let directory = tempfile::tempdir().unwrap();
    fixture(directory.path());
    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let indexed =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();
    let options = |channels| QueryOptions {
        channels,
        top_n: 10,
        max_tokens: 180,
        diagnostics: true,
        ..QueryOptions::default()
    };
    let baseline = query(
        &store,
        Some(&embedder),
        "authentication token session",
        &options(QueryChannels::evidence_only()),
    )
    .unwrap();
    let dual = query(
        &store,
        Some(&embedder),
        "authentication token session",
        &options(QueryChannels::for_embedder(Some(&embedder))),
    )
    .unwrap();

    assert!(!baseline.packet.items.is_empty());
    assert!(!dual.packet.items.is_empty());
    assert!(baseline.packet.items[0].evidence_text.contains("process"));
    assert!(dual.packet.items[0]
        .evidence_text
        .contains("refresh_session"));
    assert_ne!(
        baseline.debug.as_ref().unwrap().items[0].unit_id,
        dual.debug.as_ref().unwrap().items[0].unit_id,
        "the fixture must demonstrate routing lift at rank 1"
    );
}
