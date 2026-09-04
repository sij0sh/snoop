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
    std::fs::create_dir_all(directory.path().join("actors")).unwrap();
    std::fs::create_dir_all(directory.path().join("items")).unwrap();
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
    std::fs::write(
        directory.path().join("src/room.cpp"),
        "namespace snoop {\n\nclass Room {\n  public:\n    void admit(int guest);\n};\n\nvoid Room::admit(int guest) { count_ += guest; }\n\n}  // namespace snoop\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("actors/player.gd"),
        "class_name Player\nextends CharacterBody2D\n\n## Reduces health by the given amount.\nfunc take_damage(amount: int) -> void:\n\thealth -= amount\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("actors/player.tscn"),
        concat!(
            "[gd_scene load_steps=2 format=3]\n\n",
            "[ext_resource type=\"Script\" path=\"res://actors/player.gd\" id=\"1_player\"]\n\n",
            "[node name=\"Player\" type=\"CharacterBody2D\"]\n",
            "script = ExtResource(\"1_player\")\n",
            "speed = 300.0\n\n",
            "[node name=\"Camera\" type=\"Camera2D\" parent=\".\"]\n",
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("items/sword.tres"),
        concat!(
            "[gd_resource type=\"WeaponData\" load_steps=2 format=3]\n\n",
            "[sub_resource type=\"Gradient\" id=\"Gradient_ramp\"]\n",
            "offsets = PackedFloat32Array(0, 1)\n\n",
            "[resource]\n",
            "damage = 12\n",
        ),
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
        ("admit", "src/room.cpp"),
        ("take_damage", "actors/player.gd"),
        ("Camera", "actors/player.tscn"),
        ("Gradient_ramp", "items/sword.tres"),
    ] {
        let report = query(
            &store,
            Some(&embedder),
            symbol,
            &QueryOptions {
                channels: QueryChannels::for_embedder(Some(&embedder)),
                top_n: 25,
                max_tokens: 6_000,
                diagnostics: false,
                ..QueryOptions::default()
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

#[test]
fn tauri_bridge_commands_are_retrievable_across_the_boundary() {
    let _guard = env_lock();
    let directory = tempfile::tempdir().unwrap();
    let sessions_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src-tauri/src")).unwrap();
    std::fs::create_dir_all(directory.path().join("src")).unwrap();
    std::fs::write(
        directory.path().join("src-tauri/src/commands.rs"),
        concat!(
            "use std::fs;\n\n",
            "/// Persists an editor file on behalf of the frontend.\n",
            "#[tauri::command]\n",
            "pub fn save_file(path: String, contents: String) -> bool {\n",
            "    persist(path, contents)\n",
            "}\n\n",
            "fn persist(path: String, contents: String) -> bool {\n",
            "    fs::write(path, contents).is_ok()\n",
            "}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/api.ts"),
        concat!(
            "import { invoke } from '@tauri-apps/api/core';\n\n",
            "export function save(path: string, contents: string) {\n",
            "  return invoke<boolean>('save_file', { path, contents });\n",
            "}\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(sessions_root.path().join("empty")).unwrap();
    std::env::set_var("SNOOP_SESSIONS_ROOT", sessions_root.path());

    let mut store = Store::open_in_memory().unwrap();
    let embedder = MockEmbedder::new("mock-v1");
    index_repository_bounded(&mut store, directory.path(), Some(&embedder), None).unwrap();

    let report = query(
        &store,
        Some(&embedder),
        "where does the frontend call save_file?",
        &QueryOptions {
            channels: QueryChannels::for_embedder(Some(&embedder)),
            top_n: 25,
            max_tokens: 6_000,
            diagnostics: false,
            ..QueryOptions::default()
        },
    )
    .unwrap();

    for locator in ["src-tauri/src/commands.rs", "src/api.ts"] {
        assert!(
            report
                .packet
                .items
                .iter()
                .any(|item| item.source_locator == locator),
            "save_file must surface {locator}: {:?}",
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
