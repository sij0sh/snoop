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
fn code_symbols_are_retrievable_across_languages() {
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
    std::fs::write(
        directory.path().join("src/auth.js"),
        "export function rotate_session() {\n  return token;\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/store.go"),
        "package store\n\nfunc Save_item() {\n\tflush()\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/Auth.java"),
        "class Auth {\n  void load_session() {\n    verify();\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/Worker.cs"),
        "namespace App\n{\n    public class Worker\n    {\n        public void drain_queue()\n        {\n            flush();\n        }\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/geom.c"),
        "#include \"geom.h\"\n\nvoid scale_vector(int factor)\n{\n    apply(factor);\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(sessions_root.path().join("empty")).unwrap();
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    let outcome =
        index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    for (symbol, locator) in [
        ("rotate_token", "src/auth.py"),
        ("loadSession", "src/store.ts"),
        ("rotate_session", "src/auth.js"),
        ("Save_item", "src/store.go"),
        ("load_session", "src/Auth.java"),
        ("drain_queue", "src/Worker.cs"),
        ("scale_vector", "src/geom.c"),
    ] {
        let report = query(
            &store,
            outcome.repo_id,
            Some(&embedder),
            symbol,
            &QueryOptions {
                channels: QueryChannels::for_embedder(Some(&embedder)),
                top_n: 25,
                max_tokens: 6_000,
                diagnostics: false,
            },
        )
        .unwrap();
        assert!(
            report
                .packet
                .items
                .iter()
                .any(|item| item.source_locator == locator),
            "{symbol} must be retrievable from {locator}: {:?}",
            report
                .packet
                .items
                .iter()
                .map(|item| &item.source_locator)
                .collect::<Vec<_>>()
        );
    }

    std::env::remove_var("SNOOP_SESSIONS_ROOT");
}
