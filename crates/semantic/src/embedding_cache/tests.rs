use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Scripted {
    calls: AtomicUsize,
    digest: AtomicUsize,
    invalid: bool,
    unpinned: bool,
    recipe: AtomicUsize,
    change: bool,
    sent: Mutex<Vec<String>>,
}
impl Scripted {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            digest: AtomicUsize::new(1),
            invalid: false,
            unpinned: false,
            recipe: AtomicUsize::new(1),
            change: false,
            sent: Mutex::new(vec![]),
        }
    }
}
impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        if self.unpinned {
            return Ok(None);
        }
        Ok(Some(EmbeddingIdentity {
            endpoint: "http://127.0.0.1:11434/".into(),
            model: "fixture".into(),
            digest: format!("sha256:{:064x}", self.digest.load(Ordering::SeqCst)),
            recipe: format!("fixture@{}", self.recipe.load(Ordering::SeqCst)),
        }))
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().extend_from_slice(texts);
        if self.change {
            self.digest.fetch_add(1, Ordering::SeqCst);
        }
        Ok(texts
            .iter()
            .map(|t| {
                if self.invalid {
                    vec![f32::NAN]
                } else {
                    vec![1.0, t.bytes().map(u32::from).sum::<u32>() as f32 / 1000.0]
                }
            })
            .collect())
    }
}
fn setup() -> (tempfile::TempDir, Arc<Mutex<EmbeddingCache>>, Lease) {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("cache.db")).unwrap(),
    ));
    let lease = cache
        .lock()
        .unwrap()
        .bind_instance(&"a".repeat(32))
        .unwrap();
    (dir, cache, lease)
}

fn run(
    provider: &Scripted,
    cache: Arc<Mutex<EmbeddingCache>>,
    texts: &[String],
) -> (Vec<Embedding>, CacheStats) {
    let lease = cache.lock().unwrap().lease().unwrap();
    let wrapped = CachedProvider::new(provider, cache, lease).unwrap();
    wrapped.embed(texts).unwrap();
    wrapped.seal().unwrap();
    let result = wrapped.embed(texts).unwrap();
    (result, wrapped.finish().unwrap())
}
#[test]
fn evaluation_binding_requires_sealed_pinned_current_operation() {
    // AC-0274: reuse the actual cache namespace, never approximate model identity.
    let (_dir, cache, lease) = setup();
    let provider = Scripted::new();
    let operation = CachedProvider::new(&provider, cache.clone(), lease.clone()).unwrap();
    assert!(operation.finish_with_binding().is_err());
    operation.embed(&["query".into()]).unwrap();
    operation.seal().unwrap();
    let (_, binding) = operation.finish_with_binding().unwrap();
    let binding = binding.unwrap();
    assert_eq!(Some(binding.namespace()), operation.space.as_deref());
    assert_eq!(binding.dimensions(), 2);

    let warm = CachedProvider::new(&provider, cache.clone(), lease).unwrap();
    warm.embed(&["query".into()]).unwrap();
    warm.seal().unwrap();
    assert_eq!(warm.finish_with_binding().unwrap().1, Some(binding));
    provider.digest.fetch_add(1, Ordering::SeqCst);
    assert!(warm.finish_with_binding().is_err());
    assert!(operation.finish_with_binding().is_err());

    let mut unpinned = Scripted::new();
    unpinned.unpinned = true;
    let lease = cache.lock().unwrap().lease().unwrap();
    let operation = CachedProvider::new(&unpinned, cache.clone(), lease).unwrap();
    operation.embed(&["query".into()]).unwrap();
    operation.seal().unwrap();
    assert!(operation.finish_with_binding().unwrap().1.is_none());
    cache.lock().unwrap().invalidate().unwrap();
    assert!(operation.finish_with_binding().is_err());
}
#[test]
fn embedding_cache_reopens_deduplicates_redacts_and_reuses_exact_transmitted_text() {
    // AC-0269: persist exact post-redaction bytes, never raw source or labels.
    let (dir, cache, _) = setup();
    let provider = Scripted::new();
    let input = vec!["same".into(), "same".into(), "other".into()];
    let (cold, stats) = run(&provider, cache.clone(), &input);
    assert_eq!(cold[0], cold[1]);
    assert_eq!(stats.unique_misses, 2);
    drop(cache);
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("cache.db")).unwrap(),
    ));
    let (warm, stats) = run(&provider, cache.clone(), &input);
    assert_eq!(cold, warm);
    assert_eq!(stats.calls, 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    run(&provider, cache.clone(), &["changed".into()]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let first = "api_key=abcdefgh1234567890".to_string();
    let second = "api_key=othersecret1234567890".to_string();
    assert_eq!(
        core_redact::redact_text(&first).0,
        core_redact::redact_text(&second).0
    );
    run(&provider, cache.clone(), &[first]);
    let before = provider.calls.load(Ordering::SeqCst);
    run(&provider, cache, &[second]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), before);
    assert!(
        provider
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("REDACTED"))
    );
}
#[test]
fn embedding_cache_atomic_seal_rejects_failed_batches_changed_identity_and_stale_leases() {
    // AC-0269: even a previously successful batch is not published before seal.
    let (_dir, cache, lease) = setup();
    let mut p = Scripted::new();
    p.invalid = true;
    let wrapper = CachedProvider::new(&p, cache.clone(), lease.clone()).unwrap();
    assert!(wrapper.embed(&["one".into()]).is_err());
    drop(wrapper);
    p.invalid = false;
    let wrapper = CachedProvider::new(&p, cache.clone(), lease.clone()).unwrap();
    wrapper.embed(&["first successful batch".into()]).unwrap();
    p.digest.store(2, Ordering::SeqCst);
    assert!(wrapper.seal().is_err());
    assert_eq!(
        cache
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(wrapper);
    p.change = true;
    let wrapper = CachedProvider::new(&p, cache.clone(), lease.clone()).unwrap();
    assert!(wrapper.embed(&["one".into()]).is_err());
    drop(wrapper);
    p.change = false;
    let wrapper = CachedProvider::new(&p, cache.clone(), lease).unwrap();
    wrapper.embed(&["one".into()]).unwrap();
    cache.lock().unwrap().invalidate().unwrap();
    assert!(wrapper.seal().is_err());
    assert!(wrapper.embed(&["one".into()]).is_err());
    assert_eq!(
        cache
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn embedding_cache_model_namespace_corruption_and_input_bounds() {
    // AC-0269: mutable tags and corrupt vectors cannot yield persistent reuse.
    let (_dir, cache, _) = setup();
    let provider = Scripted::new();
    run(&provider, cache.clone(), &["one".into()]);
    provider.digest.store(2, Ordering::SeqCst);
    run(&provider, cache.clone(), &["one".into()]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    cache
        .lock()
        .unwrap()
        .connection
        .execute("UPDATE vectors SET vector=zeroblob(dimension*4)", [])
        .unwrap();
    let (_, stats) = run(&provider, cache.clone(), &["one".into()]);
    assert_eq!(stats.rejected_rows, 1);
    let lease = cache.lock().unwrap().lease().unwrap();
    let wrapper = CachedProvider::new(&provider, cache, lease).unwrap();
    assert!(wrapper.embed(&["x".repeat(MAX_TEXT_BYTES + 1)]).is_err());
}
#[test]
fn embedding_cache_first_writer_wins_and_dimension_is_pinned() {
    // AC-0269: winning bytes, rather than a concurrent loser's output, are reused.
    let (_dir, cache, lease) = setup();
    let mut store = cache.lock().unwrap();
    let key = hash(b"one");
    let space = hash(b"space");
    let protected = BTreeSet::from([(space.clone(), key.clone())]);
    let mut stats = CacheStats::default();
    store
        .publish(
            &lease,
            &space,
            &[(key.as_str(), 3, &[1., 2.][..])],
            &protected,
            &mut stats,
        )
        .unwrap();
    store
        .publish(
            &lease,
            &space,
            &[(key.as_str(), 3, &[3., 4.][..])],
            &protected,
            &mut stats,
        )
        .unwrap();
    assert_eq!(stats.conflicts, 1);
    assert_eq!(
        store.load(&lease, &space, &key, 3, &mut stats).unwrap(),
        Some(vec![1., 2.])
    );
    assert!(
        store
            .publish(
                &lease,
                &space,
                &[(hash(b"two").as_str(), 3, &[1.][..])],
                &protected,
                &mut stats
            )
            .is_err()
    );
}
#[test]
fn embedding_cache_whole_preview_reopens_zero_calls_and_recomputes_labels_and_membership() {
    // AC-0269: real evaluate/propose consume sealed winning vectors; no labels persist.
    use crate::{Candidate, HopKind, LabeledPair, UnresolvedHop};
    let (dir, cache, _) = setup();
    let provider = Scripted::new();
    let evidence=serde_json::from_value(serde_json::json!([{"repo":"fixture/repo","path":"file.ts","byte_start":0,"byte_end":1,"commit_sha":"abc"}])).unwrap();
    let hop = UnresolvedHop {
        gap_id: "gap".into(),
        source_id: "source".into(),
        edge_label: "CALLS".into(),
        kind: HopKind::Call,
        subtype: None,
        query: "same".into(),
        evidence,
        exclusion: None,
    };
    let candidate = Candidate {
        node_id: "target".into(),
        kind: HopKind::Call,
        subtype: None,
        text: "same".into(),
        evidence: hop.evidence.clone(),
        materialized_node: None,
    };
    let irrelevant = Candidate {
        node_id: "unused".into(),
        kind: HopKind::Channel,
        subtype: Some("other".into()),
        text: "must never embed".into(),
        evidence: hop.evidence.clone(),
        materialized_node: None,
    };
    let mut pairs = vec![
        LabeledPair {
            query: "same".into(),
            candidate: "same".into(),
            is_match: true,
        },
        LabeledPair {
            query: "same".into(),
            candidate: "different".into(),
            is_match: false,
        },
    ];
    let candidates = vec![candidate.clone(), irrelevant];
    let compute =
        |cache: Arc<Mutex<EmbeddingCache>>, pairs: &[LabeledPair], candidates: &[Candidate]| {
            let lease = cache.lock().unwrap().lease().unwrap();
            let wrapper = CachedProvider::new(&provider, cache, lease).unwrap();
            let inputs =
                crate::embedding_inputs(std::slice::from_ref(&hop), candidates, pairs).unwrap();
            wrapper.embed(&inputs).unwrap();
            wrapper.seal().unwrap();
            let eval = crate::evaluate(&wrapper, pairs, 0.95).unwrap();
            let proposals =
                crate::propose(&wrapper, std::slice::from_ref(&hop), candidates, 3).unwrap();
            let stats = wrapper.finish().unwrap();
            (eval, proposals, stats)
        };
    let (cold, proposals, _) = compute(cache.clone(), &pairs, &candidates);
    assert!(
        !provider
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "must never embed")
    );
    drop(cache);
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("cache.db")).unwrap(),
    ));
    let (warm, warm_proposals, stats) = compute(cache.clone(), &pairs, &candidates);
    assert_eq!(stats.calls, 0);
    assert_eq!(
        serde_json::to_vec(&cold).unwrap(),
        serde_json::to_vec(&warm).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&proposals).unwrap(),
        serde_json::to_vec(&warm_proposals).unwrap()
    );
    pairs[0].is_match = false;
    pairs[1].is_match = true;
    let (changed, _, stats) = compute(cache.clone(), &pairs, &candidates);
    assert_eq!(stats.calls, 0);
    assert_ne!(
        serde_json::to_vec(&cold).unwrap(),
        serde_json::to_vec(&changed).unwrap()
    );
    let (_, removed, stats) = compute(cache, &pairs, &[]);
    assert!(removed.is_empty());
    assert_eq!(stats.calls, 0);
}

#[test]
fn embedding_cache_eviction_is_sequence_ordered_and_preserves_current_operation() {
    // AC-0269: deterministic eviction cannot remove the current working set.
    let (_dir, cache, lease) = setup();
    let mut store = cache.lock().unwrap();
    let space = hash(b"eviction");
    store
        .connection
        .execute("INSERT INTO spaces VALUES(?1,1)", [&space])
        .unwrap();
    // A full store: exactly MAX_ROWS rows.
    store.connection.execute(&format!("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<{MAX_ROWS}) INSERT INTO vectors SELECT ?1,printf('%064x',x),1,1,X'0000803f',x FROM n"),[&space]).unwrap();
    store
        .connection
        .execute(&format!("UPDATE cache_meta SET sequence={MAX_ROWS}"), [])
        .unwrap();
    store.validate().unwrap();
    let oldest = format!("{:064x}", 1);
    let second = format!("{:064x}", 2);
    let new = hash(b"new");
    let protected = BTreeSet::from([
        (space.clone(), oldest.clone()),
        (space.clone(), new.clone()),
    ]);
    let mut stats = CacheStats::default();
    store
        .publish(
            &lease,
            &space,
            &[(new.as_str(), 3, &[2.][..])],
            &protected,
            &mut stats,
        )
        .unwrap();
    assert_eq!(stats.evictions, 1);
    assert!(
        store
            .load(&lease, &space, &oldest, 1, &mut stats)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .load(&lease, &space, &second, 1, &mut stats)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.load(&lease, &space, &new, 3, &mut stats).unwrap(),
        Some(vec![2.])
    );
}
#[test]
fn embedding_cache_unpinned_recipe_change_and_schema_admission_fail_closed() {
    // AC-0269: unknown artifact identity never persists; recipe changes miss.
    let (_dir, cache, _) = setup();
    let mut p = Scripted::new();
    p.unpinned = true;
    let (_, stats) = run(&p, cache.clone(), &["one".into()]);
    assert_eq!(stats.unpinned_identity, 1);
    run(&p, cache.clone(), &["one".into()]);
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        cache
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    p.unpinned = false;
    run(&p, cache.clone(), &["one".into()]);
    p.recipe.store(2, Ordering::SeqCst);
    let (_, stats) = run(&p, cache.clone(), &["one".into()]);
    assert_eq!(stats.calls, 1);
    cache
        .lock()
        .unwrap()
        .connection
        .execute_batch(&format!(
            "CREATE VIEW oversized AS SELECT '{}'",
            "x".repeat(5000)
        ))
        .unwrap();
    assert!(cache.lock().unwrap().validate().is_err());
}
#[test]
fn embedding_cache_failed_operation_cannot_seal_prior_staged_vectors() {
    // AC-0269: ignoring a failed later input cannot publish earlier staged work.
    let (_dir, cache, lease) = setup();
    let p = Scripted::new();
    let wrapped = CachedProvider::new(&p, cache.clone(), lease).unwrap();
    wrapped.embed(&["valid".into()]).unwrap();
    assert!(wrapped.embed(&["x".repeat(MAX_TEXT_BYTES + 1)]).is_err());
    assert!(wrapped.seal().is_err());
    assert_eq!(
        cache
            .lock()
            .unwrap()
            .connection
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn embedding_cache_disposable_recovery_rotates_epoch_and_preserves_attached_database() {
    // AC-0269: recovery can discard only the fixed main cache, never attached user data.
    let dir = tempfile::tempdir().unwrap();
    let mut cache = EmbeddingCache::open_disposable(dir.path()).unwrap();
    let old = cache.bind_instance(&"a".repeat(32)).unwrap();
    let external = dir.path().canonicalize().unwrap().join("user.db");
    cache
        .connection
        .execute(
            "ATTACH DATABASE ?1 AS external",
            [external.to_str().unwrap()],
        )
        .unwrap();
    cache.connection.execute_batch("CREATE TABLE external.important(value TEXT); INSERT INTO external.important VALUES('keep'); CREATE VIEW unexpected AS SELECT 1;").unwrap();
    cache.invalidate().unwrap();
    let new = cache.bind_instance(&"a".repeat(32)).unwrap();
    assert_ne!(old, new);
    assert_ne!(old.epoch, new.epoch);
    assert_eq!(old.generation, new.generation);
    assert!(cache.check(&old).is_err());
    assert_eq!(
        cache
            .connection
            .query_row("SELECT value FROM external.important", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}
#[test]
fn embedding_cache_fixed_path_recovers_corrupt_foreign_and_newer_schema() {
    // AC-0269: disposable storage is not a prerequisite for application availability.
    for kind in ["corrupt", "foreign", "newer"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.db");
        match kind {
            "corrupt" => std::fs::write(&path, b"not a SQLite database").unwrap(),
            "foreign" => {
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch("CREATE TABLE user_data(value); INSERT INTO user_data VALUES('foreign'); PRAGMA user_version=99;").unwrap();
            }
            _ => {
                let cache = EmbeddingCache::open(&path).unwrap();
                cache
                    .connection
                    .pragma_update(None, "user_version", 99)
                    .unwrap();
            }
        }
        let mut cache = EmbeddingCache::open_disposable(dir.path()).unwrap();
        cache.validate().unwrap();
        assert_eq!(
            cache
                .connection
                .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let lease = cache.bind_instance(&"a".repeat(32)).unwrap();
        let p = Scripted::new();
        let cache = Arc::new(Mutex::new(cache));
        let wrapper = CachedProvider::new(&p, cache, lease).unwrap();
        wrapper.embed(&["cold".into()]).unwrap();
        wrapper.seal().unwrap();
        assert_eq!(wrapper.finish().unwrap().calls, 1);
    }
}
#[test]
fn embedding_cache_ordinary_open_leaves_foreign_schema_and_version_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("user.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE precious(value); INSERT INTO precious VALUES('keep'); PRAGMA user_version=99;").unwrap();
    let before: String = conn
        .query_row("SELECT group_concat(sql,';') FROM sqlite_master", [], |r| {
            r.get(0)
        })
        .unwrap();
    drop(conn);
    assert!(EmbeddingCache::open(&path).is_err());
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT group_concat(sql,';') FROM sqlite_master", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        99
    );
}
#[test]
fn embedding_cache_counter_exhaustion_recovers_before_invalidation() {
    // AC-0269: hostile/exhausted signed counters cannot block graph or retention work.
    let dir = tempfile::tempdir().unwrap();
    let mut cache = EmbeddingCache::open_disposable(dir.path()).unwrap();
    let old = cache.bind_instance(&"a".repeat(32)).unwrap();
    cache
        .connection
        .execute_batch(
            "UPDATE cache_meta SET generation=9223372036854775807,sequence=9223372036854775807",
        )
        .unwrap();
    cache.invalidate().unwrap();
    let new = cache.bind_instance(&"a".repeat(32)).unwrap();
    assert_ne!(old.epoch, new.epoch);
    cache.validate().unwrap();
}
#[test]
fn embedding_cache_sequence_reserve_covers_a_full_plan() {
    // AC-1507: the counter reserve grows with MAX_ROWS, so a store that
    // cannot seal a full plan is exhausted (and recovered), never admitted.
    let dir = tempfile::tempdir().unwrap();
    let mut cache = EmbeddingCache::open_disposable(dir.path()).unwrap();
    cache.bind_instance(&"a".repeat(32)).unwrap();
    for (sequence, valid) in [
        (SEQUENCE_CEILING, true),
        (SEQUENCE_CEILING + 1, false),
        (i64::MAX - 100_000, false),
    ] {
        cache
            .connection
            .execute("UPDATE cache_meta SET sequence=?1", [sequence])
            .unwrap();
        assert_eq!(cache.validate().is_ok(), valid, "sequence {sequence}");
    }
    assert_eq!(
        SEQUENCE_CEILING.checked_add(MAX_ROWS as i64),
        Some(i64::MAX)
    );
}
#[test]
fn embedding_cache_namespace_capacity_evicts_oldest_unprotected_namespace() {
    // AC-0269: the 257th artifact namespace remains useful even below byte/row caps.
    let (_dir, cache, lease) = setup();
    let mut store = cache.lock().unwrap();
    let mut stats = CacheStats::default();
    let key = hash(b"one");
    for n in 0..257 {
        let space = hash(n.to_string().as_bytes());
        store
            .publish(
                &lease,
                &space,
                &[(key.as_str(), 3, &[1.][..])],
                &BTreeSet::from([(space.clone(), key.clone())]),
                &mut stats,
            )
            .unwrap();
    }
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM spaces", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        256
    );
    assert_eq!(stats.evictions, 1);
    assert!(
        store
            .load(&lease, &hash(b"0"), &key, 3, &mut stats)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .load(&lease, &hash(b"256"), &key, 3, &mut stats)
            .unwrap()
            .is_some()
    );
}
#[cfg(unix)]
#[test]
fn embedding_cache_disposable_rejects_main_and_sidecar_links() {
    use std::os::unix::fs::symlink;
    for name in [
        "embeddings.db",
        "embeddings.db-wal",
        "embeddings.db-shm",
        "embeddings.db-journal",
    ] {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let precious = dir.path().join("precious.db");
            std::fs::write(&precious, b"keep this user file").unwrap();
            let alias = dir.path().join(name);
            if hard {
                std::fs::hard_link(&precious, &alias).unwrap()
            } else {
                symlink(&precious, &alias).unwrap()
            };
            assert!(EmbeddingCache::open_disposable(dir.path()).is_err());
            assert_eq!(std::fs::read(precious).unwrap(), b"keep this user file");
        }
    }
}

#[test]
fn embedding_cache_working_set_accepts_exact_row_limit_and_rejects_next() {
    // AC-0269: capacity includes the currently admitted key without an off-by-one.
    let (_dir, cache, lease) = setup();
    let p = Scripted::new();
    let wrapper = CachedProvider::new(&p, cache, lease).unwrap();
    {
        let mut state = wrapper.state.lock().unwrap();
        for n in 0..MAX_ROWS - 1 {
            state.vectors.insert(format!("{n:064x}"), (1, vec![1., 1.]));
        }
        state.vector_bytes = (MAX_ROWS - 1) * 8;
    }
    wrapper.embed(&["final allowed key".into()]).unwrap();
    assert_eq!(wrapper.state.lock().unwrap().vectors.len(), MAX_ROWS);
    assert!(wrapper.embed(&["one key too many".into()]).is_err());
}

#[test]
fn operation_limits_bound_the_working_set_and_never_raise_it() {
    // AC-1807 / #1653: a lowered row bound admits exactly that many keys and
    // refuses the next; a lowered byte bound refuses the vectors past it; the
    // shipped bounds are the default and nothing above them is accepted.
    assert_eq!(
        OperationLimits::within(MAX_ROWS, MAX_VECTOR_BYTES),
        Some(OperationLimits::default())
    );
    for (rows, bytes) in [
        (MAX_ROWS + 1, MAX_VECTOR_BYTES),
        (MAX_ROWS, MAX_VECTOR_BYTES + 1),
        (OperationLimits::MIN_ROWS - 1, MAX_VECTOR_BYTES),
        (MAX_ROWS, OperationLimits::MIN_VECTOR_BYTES - 1),
    ] {
        assert_eq!(OperationLimits::within(rows, bytes), None);
    }
    let rows = OperationLimits::MIN_ROWS;
    let limits = OperationLimits::within(rows, MAX_VECTOR_BYTES).unwrap();
    let (_dir, cache, lease) = setup();
    let p = Scripted::new();
    let wrapper = CachedProvider::within(&p, cache.clone(), lease.clone(), limits).unwrap();
    assert_eq!(wrapper.limits(), limits);
    {
        let mut state = wrapper.state.lock().unwrap();
        for n in 0..rows - 1 {
            state.vectors.insert(format!("{n:064x}"), (1, vec![1., 1.]));
        }
        state.vector_bytes = (rows - 1) * 8;
    }
    wrapper.embed(&["final allowed key".into()]).unwrap();
    assert!(wrapper.embed(&["one key too many".into()]).is_err());

    // Repeated texts count once per input, so they cannot pass the row bound.
    let repeated = vec!["same text".to_string(); rows + 1];
    let wrapper = CachedProvider::within(&p, cache.clone(), lease.clone(), limits).unwrap();
    assert!(wrapper.embed(&repeated).is_err());
    let wrapper = CachedProvider::within(&p, cache.clone(), lease.clone(), limits).unwrap();
    assert_eq!(wrapper.embed(&repeated[..rows]).unwrap().len(), rows);

    let bytes = OperationLimits::within(MAX_ROWS, OperationLimits::MIN_VECTOR_BYTES).unwrap();
    let wrapper = CachedProvider::within(&p, cache, lease, bytes).unwrap();
    wrapper.state.lock().unwrap().vector_bytes = OperationLimits::MIN_VECTOR_BYTES - 4;
    assert!(wrapper.embed(&["past the byte bound".into()]).is_err());
}

#[test]
fn a_lowered_batch_splits_provider_calls_without_changing_vectors() {
    // AC-1819 / #1653: the texts per provider call can be lowered from 1 up
    // to the shipped batch, never raised; a smaller batch makes more calls
    // and returns the same vectors.
    let limits = OperationLimits::default();
    assert_eq!(limits.batch_texts(), BATCH_COUNT);
    assert_eq!(limits.with_batch_texts(0), None);
    assert_eq!(limits.with_batch_texts(BATCH_COUNT + 1), None);
    assert_eq!(limits.with_batch_texts(BATCH_COUNT), Some(limits));
    let small = limits.with_batch_texts(3).unwrap();
    assert_eq!(small.batch_texts(), 3);
    let texts: Vec<String> = (0..7).map(|n| format!("text {n}")).collect();
    let (_dir, cache, lease) = setup();
    let whole = Scripted::new();
    let wrapper = CachedProvider::within(&whole, cache.clone(), lease.clone(), limits).unwrap();
    let expected = wrapper.embed(&texts).unwrap();
    assert_eq!(whole.calls.load(Ordering::SeqCst), 1);
    let (_dir, cache, lease) = setup();
    let split = Scripted::new();
    let wrapper = CachedProvider::within(&split, cache, lease, small).unwrap();
    assert_eq!(wrapper.embed(&texts).unwrap(), expected);
    assert_eq!(split.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn vscode_scale_plans_are_cached_and_reused_without_calls() {
    // AC-1507: a plan past the old 100k bound is embedded once, persisted
    // whole, and a reopened warm pass makes no embedding call. The whole-VSCode
    // plan count (about 559k texts) is checked by
    // `vscode_scale_embedding_plans_are_admitted`; this test stays at 200k
    // rows to bound suite time.
    let (dir, cache, _) = setup();
    let provider = Scripted::new();
    let texts: Vec<String> = (0..200_000).map(|n| format!("symbol {n:06}")).collect();
    let (_, cold) = run(&provider, cache.clone(), &texts);
    assert_eq!(cold.unique_misses, 200_000);
    assert_eq!(cold.evictions, 0);
    drop(cache);
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("cache.db")).unwrap(),
    ));
    let calls = provider.calls.load(Ordering::SeqCst);
    let (_, warm) = run(&provider, cache, &texts);
    assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    assert_eq!(warm.unique_misses, 0);
}
