use base64::{engine::general_purpose::STANDARD, Engine};
use git_monitor::{
    config::{Config, DiffConfig, Limits},
    diff,
    model::{EventView, Report},
    split,
};

fn fixture() -> (Config, git_monitor::mail::MailConfig) {
    let mut cfg: Config = toml::from_str(include_str!("../examples/monitor.toml")).unwrap();
    cfg.defaults.max_message_bytes = 40000;
    cfg.limits.max_part_count = 100;
    let mail=toml::from_str("version=1\n[smtp]\nhost='localhost'\nport=0\nsecurity='none'\nfrom='test@example.invalid'\ntimeout='1s'\n").unwrap();
    (cfg, mail)
}
fn event() -> EventView {
    EventView {
        id: 1,
        repository: "test".into(),
        remote: "origin".into(),
        branch: "main".into(),
        kind: "branch_updated".into(),
        old: None,
        new: None,
        observed_at: 0,
        watches: vec!["scope".into()],
        commits: vec![],
        outgoing: vec![],
        files: vec![],
        notes: vec![],
    }
}

#[test]
fn repeated_bisection_preserves_allowed_bytes_unicode_and_display_spans() {
    for (old, new) in [
        (b"invalid\xffsource".as_slice(), b"next\xfevalue".as_slice()),
        ("old🦀long line".as_bytes(), "new🦋longer line".as_bytes()),
    ] {
        let file = diff::build(
            "scope.txt",
            old,
            new,
            11,
            21,
            None,
            &DiffConfig::default(),
            &Limits::default(),
        );
        let mut pieces = vec![split::begin(&file)];
        for _ in 0..4 {
            pieces = pieces
                .into_iter()
                .flat_map(|p| match split::bisect(&p) {
                    Some((a, b)) => vec![a, b],
                    None => vec![p],
                })
                .collect();
        }
        let decoded = |old: bool| {
            pieces
                .iter()
                .flat_map(|p| {
                    STANDARD
                        .decode(if old { &p.old_bytes } else { &p.new_bytes })
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(decoded(true), old);
        assert_eq!(decoded(false), new);
        for p in &pieces {
            let c = p.continuation.as_ref().unwrap();
            assert_eq!(c.new_sha256, git_monitor::hash(new));
            assert_eq!(
                STANDARD.decode(&p.new_bytes).unwrap(),
                new[c.new_range.0..c.new_range.1]
            );
        }
        let before: String = file
            .rows
            .iter()
            .flat_map(|r| &r.right)
            .map(|s| s.text.as_str())
            .collect();
        let after: String = pieces
            .iter()
            .flat_map(|p| &p.rows)
            .flat_map(|r| &r.right)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(before, after);
    }
}

#[test]
fn mime_splits_a_long_unicode_line_and_bounds_max_part_count() {
    let (mut cfg, mail) = fixture();
    let source = "🦀 & <source> ".repeat(2000);
    let file = diff::build(
        "selected.txt",
        b"",
        source.as_bytes(),
        1,
        1,
        None,
        &cfg.diff,
        &cfg.limits,
    );
    let mut e = event();
    e.files.push(file);
    let recipient = cfg.recipients.values().next().unwrap();
    let report = Report {
        recipient: "test".into(),
        generated_at: 1,
        events: vec![e.clone()],
    };
    let parts = git_monitor::mail::package(&report, recipient, &mail, &cfg).unwrap();
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|p| p.1.len() <= 40000));
    assert!(parts.iter().any(|p| p.2.contains("Continuation")));
    assert!(!parts.iter().any(|p| p.2.contains("oversized_item")));
    cfg.limits.max_part_count = 1;
    e.files.push(diff::build(
        "second.txt",
        b"",
        b"another change",
        1,
        1,
        None,
        &cfg.diff,
        &cfg.limits,
    ));
    let report = Report {
        events: vec![e],
        ..report
    };
    let parts = git_monitor::mail::package(&report, recipient, &mail, &cfg).unwrap();
    assert_eq!(parts.len(), 1);
    assert!(parts[0].1.len() <= 40000);
    assert!(parts[0].2.contains("oversized_item"));
}
