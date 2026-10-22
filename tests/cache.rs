use git_monitor::{cache, metrics};
#[test]
fn disposable_entries_validate_encoding_hash_size_and_retention() {
    let temp = tempfile::tempdir().unwrap();
    let key = "a".repeat(64);
    assert!(cache::get(temp.path(), &key, 10).is_none());
    cache::put(temp.path(), &key, b"\x00\xffbytes").unwrap();
    assert_eq!(
        cache::get(temp.path(), &key, 100).unwrap(),
        b"\x00\xffbytes"
    );
    assert!(cache::get(temp.path(), &key, 1).is_none());
    std::fs::write(temp.path().join(&key), b"corrupt").unwrap();
    assert!(cache::get(temp.path(), &key, 100).is_none());
    cache::put(temp.path(), &key, b"data").unwrap();
    assert_eq!(cache::prune(temp.path(), 0, 0).unwrap(), 1);
}
#[test]
fn operation_counts_merge_without_double_counting_process_resources() {
    metrics::reset();
    metrics::update(|c| c.git_processes = 2);
    let counts = metrics::snapshot();
    metrics::merge(&counts);
    assert_eq!(metrics::snapshot().git_processes, 4);
    metrics::log("test");
    let _: metrics::Counters = serde_json::from_str("{}").unwrap();
}
