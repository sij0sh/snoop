use super::*;
use crate::inference::{EmbedResult, Embedder};
use crate::ingest::index_embeddings;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn lease_row(store: &Store, repo: RepoId) -> Option<(String, i64)> {
    store
        .connection()
        .query_row(
            "SELECT owner,expires_at FROM index_leases WHERE repo_id=?1",
            rusqlite::params![repo.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}

fn epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

struct DelayEmbedder {
    version: &'static str,
    delay: Duration,
}

impl Embedder for DelayEmbedder {
    fn model_version(&self) -> &str {
        self.version
    }

    fn embed_query(&self, _text: &str) -> EmbedResult<Vec<f32>> {
        Ok(vec![1.0; 8])
    }

    fn embed_documents(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        std::thread::sleep(self.delay);
        Ok(texts.iter().map(|_| vec![1.0f32; 8]).collect())
    }
}

#[test]
fn index_lease_is_exclusive_owner_scoped_and_steals_expired_leases() {
    let store = Store::open_in_memory().unwrap();
    let repo = RepoId(1);
    store.ensure_repository("/repo").unwrap();

    assert!(store.acquire_lease(repo, "indexer-a", 3600).unwrap());
    assert!(
        !store.acquire_lease(repo, "indexer-b", 3600).unwrap(),
        "an unexpired lease is not acquirable"
    );

    store.release_lease(repo, "indexer-b").unwrap();
    assert!(
        !store.acquire_lease(repo, "indexer-c", 3600).unwrap(),
        "release must not remove another owner's lease"
    );

    store
        .connection()
        .execute("UPDATE index_leases SET expires_at=0", [])
        .unwrap();
    assert!(
        store.acquire_lease(repo, "indexer-b", 3600).unwrap(),
        "an expired lease is stolen lazily on acquire"
    );

    store.release_lease(repo, "indexer-b").unwrap();
    assert!(store.acquire_lease(repo, "indexer-c", 3600).unwrap());
}

#[test]
fn renew_lease_renews_only_an_unexpired_owner_lease() {
    let store = Store::open_in_memory().unwrap();
    store.ensure_repository("/repo").unwrap();
    let repo = RepoId(1);

    assert!(store.acquire_lease(repo, "indexer-a", 3).unwrap());
    let e0 = lease_row(&store, repo).unwrap().1;
    std::thread::sleep(Duration::from_millis(1200));

    assert!(
        store.renew_lease(repo, "indexer-a", 3600).unwrap(),
        "the owner renews its own unexpired lease"
    );
    let e1 = lease_row(&store, repo).unwrap().1;
    assert!(
        e1 >= e0 + 3598,
        "renewal pushes expires_at forward by the ttl (e0={e0}, e1={e1})"
    );

    assert!(
        !store.renew_lease(repo, "indexer-b", 3600).unwrap(),
        "a different owner cannot renew the lease"
    );
    assert_eq!(
        lease_row(&store, repo).unwrap().1,
        e1,
        "a failed renewal leaves expires_at untouched"
    );

    // Real wall-clock expiry: an expired lease cannot be renewed.
    store.release_lease(repo, "indexer-a").unwrap();
    assert!(store.acquire_lease(repo, "indexer-c", 1).unwrap());
    std::thread::sleep(Duration::from_millis(1300));
    assert!(
        !store.renew_lease(repo, "indexer-c", 3600).unwrap(),
        "an expired lease cannot be renewed"
    );
    assert!(
        store.acquire_lease(repo, "indexer-d", 60).unwrap(),
        "the expired lease is stolen on acquire"
    );
}

#[test]
fn concurrent_acquire_refuses_live_holder_and_steals_after_real_expiry() {
    let store = Store::open_in_memory().unwrap();
    store.ensure_repository("/repo").unwrap();
    let repo = RepoId(1);

    assert!(store.acquire_lease(repo, "indexer-a", 1).unwrap());
    assert!(
        !store.acquire_lease(repo, "indexer-b", 60).unwrap(),
        "a second indexer is refused while the holder's lease is unexpired"
    );

    std::thread::sleep(Duration::from_millis(1300));
    let (owner, expires) = lease_row(&store, repo).unwrap();
    assert_eq!(owner, "indexer-a");
    assert!(
        expires <= epoch_secs(),
        "the lease has really expired by wall clock (expires={expires}, now={})",
        epoch_secs()
    );

    assert!(
        store.acquire_lease(repo, "indexer-b", 60).unwrap(),
        "the expired lease is stolen by the waiting indexer"
    );
    let (owner, _) = lease_row(&store, repo).unwrap();
    assert_eq!(owner, "indexer-b");
}

#[test]
fn index_embeddings_renews_each_embed_request_and_aborts_on_lease_loss() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("r1.db");
    let store = Store::open(&db).unwrap();
    let repository = store.ensure_repository("/repo").unwrap();
    let repo = repository.id;

    let source_id: i64 = store
        .connection()
        .query_row(
            "INSERT INTO sources(repo_id,kind,locator,content_hash)
             VALUES (?1,'file','synthetic://r1','hash-r1') RETURNING id",
            rusqlite::params![repo.0],
            |row| row.get(0),
        )
        .unwrap();
    for index in 0..100 {
        store
            .connection()
            .execute(
                "INSERT INTO retrieval_units(repo_id,source_id,kind,evidence_text,routing_text,token_count,content_hash)
                 VALUES (?1,?2,'evidence',?3,?3,3,?4)",
                rusqlite::params![
                    repo.0,
                    source_id,
                    format!("unit {index} evidence"),
                    format!("hash-{index}")
                ],
            )
            .unwrap();
    }
    assert_eq!(
        store
            .units_missing_vectors(repo, "evidence", "delay-v2")
            .unwrap()
            .len(),
        100,
        "all units are missing vectors for the delay embedder"
    );

    assert!(store.acquire_lease(repo, "index-test-a", 3600).unwrap());

    let worker = std::thread::spawn(move || {
        let embedder = DelayEmbedder {
            version: "delay-v2",
            delay: Duration::from_secs(3),
        };
        index_embeddings(&store, repo, &embedder, None, "index-test-a")
    });

    let observer = Store::open(&db).unwrap();
    let started = Instant::now();
    let mut sighting = None;
    while sighting.is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "lease row never appeared"
        );
        sighting = lease_row(&observer, repo);
        std::thread::sleep(Duration::from_millis(100));
    }
    let e0 = sighting.unwrap().1;

    // Renewal cadence: mid multi-chunk embed, expires_at has moved forward.
    std::thread::sleep(Duration::from_secs(4));
    let e1 = lease_row(&observer, repo).unwrap().1;
    assert!(
        e1 >= e0 + 2,
        "expires_at advanced during the embed phase, so renewals run between embed requests (e0={e0}, e1={e1})"
    );

    // A second indexer is refused while the first is alive mid-embed.
    assert!(
        !observer.acquire_lease(repo, "probe", 600).unwrap(),
        "a second indexer is refused while the first holds an unexpired, renewed lease"
    );

    // Scale the TTL horizon: shrink expires_at to now+1 mid-sleep; the next
    // expiry then lands between two embed requests by real wall clock.
    std::thread::sleep(Duration::from_millis(300));
    observer
        .connection()
        .execute(
            "UPDATE index_leases SET expires_at=?1+1 WHERE repo_id=?2",
            rusqlite::params![epoch_secs(), repo.0],
        )
        .unwrap();

    loop {
        let (_, expires) = lease_row(&observer, repo).unwrap();
        if expires <= epoch_secs() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "lease never reached wall-clock expiry"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        observer.acquire_lease(repo, "takeover", 600).unwrap(),
        "the lease expired by real wall clock and is stolen"
    );

    // The first indexer must abort at its next renewal, not keep writing.
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        error.to_string().contains("lease lost"),
        "the first indexer aborts when it loses the lease, got: {error}"
    );
    let (owner, _) = lease_row(&observer, repo).unwrap();
    assert_eq!(owner, "takeover", "the takeover indexer owns the lease");
}
