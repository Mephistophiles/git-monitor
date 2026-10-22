use git_monitor::{config::Limits, diff, structural};
#[test]
fn corpus_preserves_bytes_and_deterministic_node_matches() {
    for (path, old, new) in [
        (
            "x.c",
            "int a(){return 1;}\nint b(){return 2;}",
            "int b(){return 2;}\nint a(){return 3;}",
        ),
        (
            "x.cpp",
            "namespace n {int f(int x){return x;} double f(double x){return x;}}",
            "namespace n {int f(int x){return x+1;} double f(double x){return x;}}",
        ),
        (
            "x.rs",
            "fn f(x:i32)->i32{x+1}\nfn g(){f(1);}",
            "fn g(){f(2);}\nfn f(x:i32)->i32{x+1}",
        ),
        (
            "x.py",
            "def f(x):\n    if x:\n        return 1\n    return 2\n",
            "def f(x):\n    if x:\n        return 2\n    return 1\n",
        ),
        (
            "x.json",
            "{\"a\":[1,2],\"b\":true}",
            "{\"a\":[2,1],\"b\":false}",
        ),
        (
            "x.c",
            "int f(){return 1;}\nint f2(){return 1;}",
            "int f2(){return 1;}\nint f(){return 1;}",
        ),
    ] {
        let a = diff::build(
            path,
            old.as_bytes(),
            new.as_bytes(),
            20,
            40,
            None,
            &Default::default(),
            &Default::default(),
        );
        a.validate().unwrap();
        assert_eq!(a.engine, "syntax", "{}", a.note);
        assert!(!a.document.as_ref().unwrap().syntax_matches.is_empty());
        assert_eq!(
            a.document
                .as_ref()
                .unwrap()
                .reconstruct(old.as_bytes(), new.as_bytes())
                .unwrap(),
            new.as_bytes()
        );
        let b = diff::build(
            path,
            old.as_bytes(),
            new.as_bytes(),
            20,
            40,
            None,
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }
}
#[test]
fn cache_corruption_and_parse_budgets_fall_back_explicitly() {
    let temp = tempfile::tempdir().unwrap();
    structural::set_cache(Some(temp.path().into()));
    let (old, new) = (
        b"int f(){return 1;}".as_slice(),
        b"int f(){return 2;}".as_slice(),
    );
    let a = structural::compare("x.c", old, new, &Limits::default()).unwrap();
    let b = structural::compare("x.c", old, new, &Limits::default()).unwrap();
    assert_eq!(
        serde_json::to_value(a).unwrap(),
        serde_json::to_value(b).unwrap()
    );
    for entry in std::fs::read_dir(temp.path()).unwrap() {
        std::fs::write(entry.unwrap().path(), b"broken").unwrap();
    }
    assert!(structural::compare("x.c", old, new, &Limits::default()).is_ok());
    for limits in [
        Limits {
            max_syntax_nodes: 1,
            ..Default::default()
        },
        Limits {
            max_syntax_candidates: 1,
            ..Default::default()
        },
        Limits {
            syntax_timeout: "0s".into(),
            ..Default::default()
        },
    ] {
        assert!(structural::compare("x.c", old, new, &limits)
            .unwrap_err()
            .contains("budget"));
    }
    assert!(
        structural::compare("x.c", b"int f( {", new, &Limits::default())
            .unwrap_err()
            .contains("parse")
    );
    assert!(structural::compare("x.c", b"\xff", new, &Limits::default())
        .unwrap_err()
        .contains("utf8"));
    assert!(structural::compare("x.unknown", old, new, &Limits::default()).is_err());
    structural::set_cache(None);
}
