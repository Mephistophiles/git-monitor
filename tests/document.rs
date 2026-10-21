use git_monitor::{
    config::{Config, DiffConfig, Limits},
    diff,
    document::{ByteRange, DiffDocument},
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn byte_edits_reconstruct_any_allowed_content(old in proptest::collection::vec(any::<u8>(),0..512),new in proptest::collection::vec(any::<u8>(),0..512),budget in 0usize..4096) {
        let doc=DiffDocument::build("selected",&old,&new,7,19,budget);
        prop_assert_eq!(doc.reconstruct(&old,&new).unwrap(),new);
    }
    #[test]
    fn filter_set_permutations_and_duplicate_exclusions_share_identity(values in proptest::collection::vec("[a-z]{1,8}\\.c",0..20)) {
        let cfg:Config=toml::from_str(include_str!("../examples/monitor.toml")).unwrap();
        let mut a=cfg.watches[0].clone();a.exclude_paths=values.clone();a.paths=Some(values.clone());
        let mut b=a.clone();b.exclude_paths.reverse();b.exclude_paths.extend(values.iter().cloned());b.paths.as_mut().unwrap().reverse();
        prop_assert_eq!(a.selector_key(),b.selector_key());
    }
}

#[test]
fn document_rejects_scope_escape_and_tampered_sources() {
    let old = b"selected old\n";
    let new = b"selected new\n";
    let mut doc = DiffDocument::build("f.c", old, new, 1, 1, 1000);
    doc.old.original_range = ByteRange {
        start: 100,
        end: 100 + old.len(),
    };
    assert_eq!(doc.reconstruct(old, new).unwrap(), new);
    assert!(doc.reconstruct(b"private data", new).is_err());
    doc.edits[0].old.end = old.len() + 1;
    assert!(doc.reconstruct(old, new).is_err());
}

#[test]
fn similar_lines_align_around_unrelated_insertions() {
    let rows = diff::text_rows(
        "return old_value;\n",
        "unrelated();\nreturn new_value;\n",
        1,
        1,
        &DiffConfig::default(),
        &Limits::default(),
    );
    let changed = rows.iter().find(|r| r.kind == "replace").unwrap();
    assert_eq!(changed.old, Some(1));
    assert_eq!(changed.new, Some(2));
    let rows = diff::text_rows(
        "remove_this();\n",
        "completely_other();\n",
        1,
        1,
        &DiffConfig::default(),
        &Limits::default(),
    );
    assert!(!rows.iter().any(|r| r.kind == "replace"));
}

#[test]
fn grapheme_diff_keeps_combining_sequences_in_one_change() {
    let rows = diff::text_rows(
        "return e\u{301};\n",
        "return a\u{301};\n",
        1,
        1,
        &DiffConfig::default(),
        &Limits::default(),
    );
    let row = rows.iter().find(|r| r.kind == "replace").unwrap();
    assert!(row
        .left
        .iter()
        .any(|s| s.changed && s.text.contains("e\u{301}")));
    assert!(row
        .right
        .iter()
        .any(|s| s.changed && s.text.contains("a\u{301}")));
}

#[test]
fn moved_blocks_have_one_deterministic_identifier() {
    let patch=b"@@ -1,2 +1,0 @@\n-first_unique();\n-second_unique();\n@@ -10,0 +9,2 @@\n+first_unique();\n+second_unique();\n";
    let rows = diff::patch_rows(patch, &DiffConfig::default(), &Limits::default());
    let moves: Vec<_> = rows.iter().filter_map(|r| r.moved).collect();
    assert_eq!(moves.len(), 4);
    assert!(moves.iter().all(|id| *id == moves[0]));
}

#[test]
fn patch_parser_accepts_malformed_unicode_headers_without_panicking() {
    let rows = diff::patch_rows(
        "@@ α β @@\n+text\n".as_bytes(),
        &DiffConfig::default(),
        &Limits::default(),
    );
    assert!(!rows.is_empty());
}
