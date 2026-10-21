//! AC-0269: production preview and graph/retention lifecycle, entirely offline.
use super::*;
use llm::{Embedding, EmbeddingIdentity, Locality, ProviderCaps, ProviderError};
use semantic::embedding_cache::{CachedProvider, EmbeddingCache};
use std::sync::atomic::AtomicUsize;
struct Scripted(AtomicUsize);
impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "scripted-semantic"
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
        Ok(Some(EmbeddingIdentity {
            endpoint: "http://127.0.0.1:11434/".into(),
            model: "test".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            recipe: "fixture@1".into(),
        }))
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|t| vec![1., if t == "same" { 1. } else { 0. }])
            .collect())
    }
}
fn pairs() -> Vec<semantic::LabeledPair> {
    vec![
        semantic::LabeledPair {
            query: "same".into(),
            candidate: "same".into(),
            is_match: true,
        },
        semantic::LabeledPair {
            query: "same".into(),
            candidate: "different".into(),
            is_match: false,
        },
    ]
}
fn graph(path: &std::path::Path, cache: &Arc<Mutex<EmbeddingCache>>) -> SqliteGraphStore {
    let invalidator = cache.clone();
    SqliteGraphStore::open_with_reset_hook(
        path,
        Box::new(move || {
            invalidator
                .lock()
                .unwrap()
                .invalidate()
                .map_err(|e| e.to_string())
        }),
    )
    .unwrap()
}
#[test]
fn semantic_cached_preview_reopens_zero_embedding_calls_and_reset_blocks_publication() {
    let dir = tempfile::tempdir().unwrap();
    let graph_path = dir.path().join("graph.db");
    let cache_path = dir.path().join("embeddings.db");
    let cache = Arc::new(Mutex::new(EmbeddingCache::open(&cache_path).unwrap()));
    let graph = Mutex::new(graph(&graph_path, &cache));
    let lease = cache
        .lock()
        .unwrap()
        .bind_instance(&graph.lock().unwrap().instance_id().unwrap())
        .unwrap();
    let p = Scripted(AtomicUsize::new(0));
    let cold =
        build_cached_semantic_preview(&p, cache.clone(), lease.clone(), &[], &[], &pairs(), 0.95)
            .unwrap();
    assert!(cold.embedding_cache.as_ref().unwrap().calls > 0);
    let jobs = Mutex::new(JobStore::open(dir.path().join("state.db")).unwrap());
    let cold = record_semantic_preview(&graph, &jobs, &cache, &lease, cold).unwrap();
    drop(graph);
    drop(cache);
    let cache = Arc::new(Mutex::new(EmbeddingCache::open(&cache_path).unwrap()));
    let graph = Mutex::new(self::graph(&graph_path, &cache));
    let lease = cache
        .lock()
        .unwrap()
        .bind_instance(&graph.lock().unwrap().instance_id().unwrap())
        .unwrap();
    let warm =
        build_cached_semantic_preview(&p, cache.clone(), lease.clone(), &[], &[], &pairs(), 0.95)
            .unwrap();
    assert_eq!(warm.embedding_cache.as_ref().unwrap().calls, 0);
    assert_eq!(
        serde_json::to_vec(&cold.eval).unwrap(),
        serde_json::to_vec(&warm.eval).unwrap()
    );
    assert_eq!(p.0.load(Ordering::SeqCst), 1);
    // Actual reset runs after snapshot/computation but before journal publication.
    clear_graph_store(&mut graph.lock().unwrap()).unwrap();
    assert!(record_semantic_preview(&graph, &jobs, &cache, &lease, warm).is_err());
    assert_eq!(jobs.lock().unwrap().list_evals().unwrap().len(), 1);
    assert_eq!(
        rusqlite::Connection::open(cache_path)
            .unwrap()
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn semantic_cache_schema_reset_and_recreated_graph_invalidate_instance_and_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("embeddings.db")).unwrap(),
    ));
    let store = graph(&path, &cache);
    let instance = store.instance_id().unwrap();
    let lease = cache.lock().unwrap().bind_instance(&instance).unwrap();
    let p = Scripted(AtomicUsize::new(0));
    let wrapped = CachedProvider::new(&p, cache.clone(), lease.clone()).unwrap();
    wrapped.embed(&["one".into()]).unwrap();
    wrapped.seal().unwrap();
    drop(wrapped);
    drop(store);
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 0)
        .unwrap();
    let store = graph(&path, &cache);
    assert_eq!(store.instance_id().unwrap(), instance);
    assert_ne!(cache.lock().unwrap().lease().unwrap(), lease);
    drop(store);
    std::fs::remove_file(&path).unwrap();
    let store = graph(&path, &cache);
    assert_ne!(store.instance_id().unwrap(), instance);
    cache
        .lock()
        .unwrap()
        .bind_instance(&store.instance_id().unwrap())
        .unwrap();
    assert_ne!(cache.lock().unwrap().lease().unwrap(), lease);
}
#[test]
fn semantic_cache_retained_source_forget_invalidates_but_preserves_graph_facts() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(
        target.join("file.ts"),
        "export function hello() {return 1;}\n",
    )
    .unwrap();
    let mut state = crate::registered_source_tests::app_state(dir.path());
    let cache = Arc::new(Mutex::new(
        EmbeddingCache::open(dir.path().join("embeddings.db")).unwrap(),
    ));
    state.graph = Mutex::new(graph(&dir.path().join("graph.db"), &cache));
    let source = state
        .sources
        .lock()
        .unwrap()
        .register_local(&target)
        .unwrap();
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &["client".into()])
        .unwrap();
    assert!(input.capture.is_some());
    drop(input);
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "retained-fact".into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    let lease = cache
        .lock()
        .unwrap()
        .bind_instance(&state.graph.lock().unwrap().instance_id().unwrap())
        .unwrap();
    let p = Scripted(AtomicUsize::new(0));
    let wrapper = CachedProvider::new(&p, cache.clone(), lease.clone()).unwrap();
    wrapper.embed(&["one".into()]).unwrap();
    wrapper.seal().unwrap();
    drop(wrapper);
    assert!(primary_source::forget(&state, &source.source_id, "stale").is_err());
    assert_eq!(cache.lock().unwrap().lease().unwrap(), lease);
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
    assert_ne!(cache.lock().unwrap().lease().unwrap(), lease);
    assert_eq!(state.graph.lock().unwrap().all_nodes().unwrap().len(), 1);
    assert_eq!(
        primary_source::preview(&state, &source.source_id)
            .unwrap()
            .captures,
        0
    );
}
