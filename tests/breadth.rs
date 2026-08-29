use std::sync::{Mutex, OnceLock};

use snoop::inference::MockEmbedder;
use snoop::ingest::index_repository_bounded;
use snoop::runtime::{query, QueryChannels, QueryOptions};
use snoop::store::Store;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}


#[test]
fn python_and_typescript_symbols_are_retrievable() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src/auth.py"),
        "def rotate_token(token):\n    validate(token)\n    return refresh(token)\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/store.ts"),
        "export function loadSession(id: string): Session {\n  return fetch(id);\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(sessions_root.path().join("empty")).unwrap();
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome = index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let report = query(
        &store,
        outcome.repo_id,
        Some(&embedder),
        "rotate_token",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 25,
            max_tokens: 6_000,
        },
    )
    .unwrap();
    assert!(
        report
            .packet
            .items
            .iter()
            .any(|item| item.source_locator == "src/auth.py"),
        "python symbol must be retrievable: {:?}",
        report
            .packet
            .items
            .iter()
            .map(|item| &item.source_locator)
            .collect::<Vec<_>>()
    );

    let ts_report = query(
        &store,
        outcome.repo_id,
        Some(&embedder),
        "loadSession",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 25,
            max_tokens: 6_000,
        },
    )
    .unwrap();
    assert!(
        ts_report
            .packet
            .items
            .iter()
            .any(|item| item.source_locator == "src/store.ts"),
        "typescript symbol must be retrievable"
    );

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}

