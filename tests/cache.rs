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
#[test]
fn render_fragments_rebind_file_anchors_and_preserve_report_bindings() {
    use git_monitor::{
        diff,
        model::{CommitView, EventView, Report},
        render,
    };
    let temp = tempfile::tempdir().unwrap();
    render::set_cache(Some(temp.path().into()));
    metrics::reset();
    let file = diff::build(
        "test.c",
        b"int x=1;",
        b"int x=2;",
        1,
        1,
        None,
        &Default::default(),
        &Default::default(),
    );
    let mut report = Report {
        recipient: "alice".into(),
        generated_at: 1,
        events: vec![EventView {
            id: 1,
            repository: "repo".into(),
            remote: "origin".into(),
            branch: "main".into(),
            kind: "branch_updated".into(),
            old: None,
            new: None,
            observed_at: 1,
            comparison_base: None,
            watches: vec!["first".into()],
            commits: vec![CommitView {
                oid: "a".repeat(40),
                subject: "subject".into(),
                author: "author".into(),
                files: vec![file.clone(), file],
            }],
            files: vec![],
            notes: vec![],
            outgoing: vec![],
        }],
    };
    let first = render::html(&report, &Default::default());
    report.recipient = "bob".into();
    report.events[0].watches = vec!["second".into()];
    let second = render::html(&report, &Default::default());
    assert!(first.contains("alice"));
    assert!(second.contains("bob"));
    assert!(second.contains("second"));
    assert!(second.contains("id=\"file-2\""));
    assert!(metrics::snapshot().render_cache_hits >= 2);
    render::set_cache(None);
}
