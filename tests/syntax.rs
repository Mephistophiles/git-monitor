use git_monitor::{
    config::{DiffConfig, Limits},
    diff,
};

fn formatting_only(path: &str, old: &str, new: &str) -> bool {
    let file = diff::build(
        path,
        old.as_bytes(),
        new.as_bytes(),
        1,
        1,
        None,
        &DiffConfig::default(),
        &Limits::default(),
    );
    assert_eq!(file.engine, "syntax", "{}", file.note);
    file.syntax.unwrap().text_only_change
}

#[test]
fn python_block_structure_changes_are_not_formatting() {
    assert!(!formatting_only(
        "f.py",
        "def f(x):\n    if x:\n        print(x)\n    return 1\n",
        "def f(x):\n    if x:\n        print(x)\n        return 1\n"
    ));
    // Equal token lists and node counts, but different nesting.
    assert!(!formatting_only(
        "f.py",
        "if a:\n    if b:\n        f()\ng()\n",
        "if a:\n    if b:\n        f()\n    g()\n"
    ));
}

#[test]
fn whitespace_and_comments_remain_formatting_only() {
    for (path, old, new) in [
        (
            "x.py",
            "def f():\n    return 1\n",
            "# comment\ndef f():\n  # more\n  return 1\n",
        ),
        (
            "x.c",
            "int f() { return 1; }\n",
            "/* comment */ int f() {\n  return 1;\n}\n",
        ),
        (
            "x.rs",
            "fn f() { let x = 1; }",
            "// comment\nfn f() {let x=1;}",
        ),
        ("x.json", "{\"x\":1}", "{ \"x\" : 1 }"),
    ] {
        assert!(formatting_only(path, old, new), "{path}");
    }
    assert!(!formatting_only("x.py", "x = 1", "x = 2"));
}
