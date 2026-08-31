//! Unit tests for the ingest boundary: transient per-file IO races skip,
//! produce errors stay run-fatal (defect-audit 20260831023057-8ecdc8ca c3).

use super::*;

fn scanned(path: &std::path::Path, content: &str) -> scanner::ScannedSource {
    scanner::ScannedSource {
        path: path.to_path_buf(),
        locator: path.file_name().unwrap().to_string_lossy().to_string(),
        kind: SourceKind::Text,
        content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
        modified_at: None,
    }
}

#[test]
fn read_source_bytes_reads_a_stable_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.txt");
    std::fs::write(&path, "stable bytes").unwrap();
    let source = scanned(&path, "stable bytes");
    match read_source_bytes(&source).unwrap() {
        SourceRead::Bytes(bytes) => assert_eq!(bytes, b"stable bytes"),
        SourceRead::Skip(reason) => panic!("unexpected skip: {reason}"),
    }
}

#[test]
fn read_source_bytes_skips_a_vanished_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = scanned(&directory.path().join("gone.txt"), "never written");
    match read_source_bytes(&source).unwrap() {
        SourceRead::Skip(reason) => assert!(reason.contains("unreadable")),
        SourceRead::Bytes(_) => panic!("a vanished file must skip"),
    }
}

#[test]
fn read_source_bytes_skips_an_unreadable_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("locked.txt");
    std::fs::write(&path, "secret").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o000);
    std::fs::set_permissions(&path, permissions).unwrap();
    let source = scanned(&path, "secret");
    let outcome = read_source_bytes(&source).unwrap();
    assert!(
        matches!(outcome, SourceRead::Skip(_)),
        "a chmod-000 file must skip, not error"
    );
}

#[test]
fn read_source_bytes_skips_a_file_that_grew_past_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("big.txt");
    let big = "x".repeat(scanner::MAX_SOURCE_BYTES as usize + 1);
    std::fs::write(&path, &big).unwrap();
    let source = scanned(&path, "tiny");
    match read_source_bytes(&source).unwrap() {
        SourceRead::Skip(reason) => assert!(reason.contains("size limit")),
        SourceRead::Bytes(_) => panic!("an oversized file must skip"),
    }
}

#[test]
fn read_source_bytes_skips_a_file_that_changed_since_scan() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("moved.txt");
    std::fs::write(&path, "new content").unwrap();
    let source = scanned(&path, "old content");
    match read_source_bytes(&source).unwrap() {
        SourceRead::Skip(reason) => assert!(reason.contains("changed")),
        SourceRead::Bytes(_) => panic!("a changed file must skip"),
    }
}

#[test]
fn produce_errors_still_abort_the_run() {
    // c3 boundary: transient file IO maps to skips, but a produce error from
    // a non-race source (bad parser output, database fault) stays run-fatal
    // so real failures are never silently swallowed.
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let root = directory.path().canonicalize().unwrap();
    store.bind_repository(&root.to_string_lossy()).unwrap();
    let mut outcome = IndexOutcome::default();
    let error = ingest_candidate(
        &mut store,
        SourceCandidate {
            kind: SourceKind::Text,
            locator: "note.txt".to_string(),
            content_hash: "hash".to_string(),
            modified_at: None,
        },
        false,
        None,
        &mut outcome,
        |_| Err("database exploded".into()),
    )
    .err()
    .expect("a produce error must stay run-fatal");
    assert!(error.to_string().contains("database exploded"));
    assert_eq!(outcome.changed_sources, 0);
}
