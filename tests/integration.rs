use git_monitor::{
    config::Config,
    git::{repository_path, Git},
    model::Report,
    process::Shutdown,
    store::{source_key, Attempt, Store},
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_git-monitor");
const BEFORE:&str="int target(int value) {\n    return value > 0;\n}\n\nint PRIVATE_NEIGHBOR(void) {\n    return 900;\n}\n";
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    remote: PathBuf,
    config: PathBuf,
}
fn git(path: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_owned()
}
impl Fixture {
    fn new(function: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let work = root.join("work");
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "user.name", "Test Author"]);
        git(&work, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(work.join("test.c"), BEFORE).unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "Initial"]);
        git(&root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
        git(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&work, &["push", "-q", "origin", "main"]);
        let config = root.join("monitor.toml");
        let selector = if function {
            "function = { path = 'test.c', match = 'target' }"
        } else {
            "paths = ['**/*.c', '*.c']"
        };
        let text=format!("version=1\n[storage]\nrepositories_dir='repos'\nstate_dir='state'\n[defaults]\npoll_every='1s'\ndigest_every='0s'\n[execution]\ngit_timeout='10s'\nshutdown_grace='200ms'\n[recipients.alice]\nto=['alice@example.invalid']\n[repositories.project.remotes.origin]\nurl='{}'\n[[watches]]\nid='runtime'\nrepository='project'\nremotes=['origin']\nrecipients=['alice']\nbranches={{all=true}}\n{selector}\n",remote.display());
        std::fs::write(&config, text).unwrap();
        Self {
            _tmp: tmp,
            root,
            work,
            remote,
            config,
        }
    }
    fn run(&self) -> Output {
        Command::new(BIN)
            .args([
                "--config",
                self.config.to_str().unwrap(),
                "run",
                "--once",
                "--force",
            ])
            .output()
            .unwrap()
    }
    fn ok(&self) {
        let o = self.run();
        assert!(
            o.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    fn change(&self, text: &str, message: &str) {
        std::fs::write(self.work.join("test.c"), text).unwrap();
        git(&self.work, &["add", "."]);
        git(&self.work, &["commit", "-qm", message]);
        git(&self.work, &["push", "-q", "origin", "main"]);
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.join("state/state.sqlite3")).unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn report(&self) -> String {
        std::fs::read_to_string(self.root.join("state/artifacts/alice-latest.html")).unwrap()
    }
}

#[test]
fn bootstrap_noop_and_incremental() {
    let f = Fixture::new(false);
    f.ok();
    assert_eq!(f.count("events"), 0);
    f.ok();
    assert_eq!(f.count("jobs"), 0);
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Fix boundary");
    f.ok();
    assert_eq!(f.count("events"), 1);
    assert_eq!(f.count("matches"), 1);
    assert!(f.report().contains("Fix boundary"));
    f.ok();
    assert_eq!(f.count("events"), 1);
    assert_eq!(f.count("matches"), 1);
}

#[test]
fn function_scope_excludes_neighbor() {
    let f = Fixture::new(true);
    f.ok();
    f.change(
        &BEFORE
            .replace("value > 0", "value >= 0")
            .replace("return 900", "return 901"),
        "Scoped change",
    );
    f.ok();
    let html = f.report();
    assert!(html.contains("Scoped change"));
    assert!(!html.contains("PRIVATE_NEIGHBOR"));
    let body: String = f
        .db()
        .query_row("SELECT body FROM matches", [], |r| r.get(0))
        .unwrap();
    assert!(!body.contains("PRIVATE_NEIGHBOR"));
    assert!(body.contains("function_modified"));
}

#[test]
fn function_delete_restore_preserves_intermediate_events() {
    let f = Fixture::new(true);
    f.ok();
    f.change(
        "int PRIVATE_NEIGHBOR(void) {\n return 900;\n}\n",
        "Remove function",
    );
    f.change(BEFORE, "Restore function");
    f.ok();
    let body: String = f
        .db()
        .query_row("SELECT body FROM matches", [], |r| r.get(0))
        .unwrap();
    assert!(body.contains("function_disappeared"));
    assert!(body.contains("function_appeared"));
    assert!(!f.report().contains("analysis_failed"));
}

#[test]
fn neighboring_change_does_not_notify_function_watch() {
    let f = Fixture::new(true);
    f.ok();
    f.change(&BEFORE.replace("return 900", "return 901"), "Neighbor only");
    f.ok();
    assert_eq!(f.count("matches"), 0);
}

#[test]
fn attributes_without_source_edit_recheck_function() {
    let f = Fixture::new(true);
    f.ok();
    std::fs::write(f.work.join(".gitattributes"), "*.c diff=python\n").unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["commit", "-qm", "Change rules"]);
    git(&f.work, &["push", "-q", "origin", "main"]);
    f.ok();
    assert!(f.report().contains("analysis_rules_changed"));
    assert!(!f.report().contains("analysis_failed"));
}

#[test]
fn branch_creation_deletion_and_empty_remote() {
    let f = Fixture::new(false);
    f.ok();
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/test"],
    );
    f.ok();
    assert!(f.report().contains("branch created"));
    git(
        &f.work,
        &[
            "push",
            "-q",
            "origin",
            ":refs/heads/release/test",
            ":refs/heads/main",
        ],
    );
    f.ok();
    assert_eq!(f.count("events"), 3);
    assert!(f.report().contains("branch deleted"));
}

#[test]
fn rollback_has_outgoing_history() {
    let f = Fixture::new(false);
    f.ok();
    let a = git(&f.work, &["rev-parse", "HEAD"]);
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Boundary");
    f.ok();
    git(
        &f.work,
        &[
            "push",
            "--force",
            "-q",
            "origin",
            &format!("{a}:refs/heads/main"),
        ],
    );
    f.ok();
    assert!(f.report().contains("history rewritten"));
    let body: String = f
        .db()
        .query_row(
            "SELECT body FROM matches ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !serde_json::from_str::<git_monitor::model::EventView>(&body)
            .unwrap()
            .outgoing
            .is_empty()
    );
}

#[test]
fn recovery_precedes_refetch_after_remote_rollback() {
    let f = Fixture::new(false);
    f.ok();
    let cfg = Config::load(&f.config).unwrap();
    let key = source_key("project", "origin", f.remote.to_str().unwrap());
    let store = Store::open(&cfg.storage.state_dir).unwrap();
    let (old, previous, _) = store.snapshot(&key).unwrap().unwrap();
    let git = Git {
        path: repository_path(&cfg.storage.repositories_dir, "project"),
        execution: cfg.execution.clone(),
        limits: cfg.limits.clone(),
        shutdown: Shutdown::default(),
    };
    let a = old["main"].clone();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Before crash");
    let attempt = Attempt {
        key,
        repository: "project".into(),
        remote: "origin".into(),
        path: git.path.clone(),
        published: old.clone(),
        old,
        initialized: true,
        watches: cfg.watches.clone(),
        previous,
        execution: cfg.execution.clone(),
        limits: cfg.limits.clone(),
        diff: cfg.diff.clone(),
    };
    store.begin(&attempt).unwrap();
    for oid in attempt.old.values() {
        git.pin(oid).unwrap();
    }
    git.fetch("origin", false).unwrap();
    drop(store);
    git::rollback(&f.work, &a);
    f.ok();
    assert_eq!(f.count("events"), 2);
    assert_eq!(f.count("attempts"), 0);
}
mod git {
    pub fn rollback(work: &std::path::Path, a: &str) {
        super::git(
            work,
            &[
                "push",
                "--force",
                "-q",
                "origin",
                &format!("{a}:refs/heads/main"),
            ],
        );
    }
}

#[test]
fn duplicate_filters_and_remotes_share_analysis() {
    let f = Fixture::new(false);
    let text = std::fs::read_to_string(&f.config).unwrap();
    let text = text.replace("remotes=['origin']", "remotes=['origin','upstream']");
    let insertion = format!(
        "\n[repositories.project.remotes.upstream]\nurl='{}'\n",
        f.remote.display()
    );
    let text = text.replace("[[watches]]", &format!("{insertion}\n[[watches]]"));
    let duplicate = text
        .split("[[watches]]")
        .nth(1)
        .unwrap()
        .replace("id='runtime'", "id='duplicate'");
    std::fs::write(&f.config, format!("{text}\n[[watches]]{duplicate}")).unwrap();
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Shared");
    let o = f.run();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("analyses=1"));
    assert_eq!(f.count("events"), 2);
    assert_eq!(f.count("matches"), 4);
    assert_eq!(f.count("cache"), 1);
}

#[test]
fn inaccessible_remote_does_not_rollback_successful_source() {
    let f = Fixture::new(false);
    let text=std::fs::read_to_string(&f.config).unwrap().replace("remotes=['origin']","remotes=['origin','broken']").replace("[[watches]]","[repositories.project.remotes.broken]\nurl='/nonexistent/git-monitor-test-remote'\n[[watches]]");
    std::fs::write(&f.config, text).unwrap();
    let o = f.run();
    assert!(!o.status.success());
    assert_eq!(f.count("snapshots"), 1);
    assert_eq!(f.count("events"), 0);
    assert_eq!(f.count("attempts"), 0);
}

#[test]
fn additive_expansion_keeps_pending_matches() {
    let f = Fixture::new(false);
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Pending result");
    f.ok();
    let text = std::fs::read_to_string(&f.config).unwrap().replace(
        "paths = ['**/*.c', '*.c']",
        "paths = ['**/*.c', '*.c', '*.rs']",
    );
    std::fs::write(&f.config, text).unwrap();
    f.ok();
    assert!(f.report().contains("Pending result"));
    assert_eq!(f.count("matches"), 1);
}

#[test]
fn html_escapes_untrusted_text_and_preserves_export() {
    let f = Fixture::new(false);
    f.ok();
    f.change(
        "const char *x = \"</script><img src=x onerror=alert(1)>\";\n",
        "<script>alert(1)</script>",
    );
    f.ok();
    let html = f.report();
    assert!(!html.contains("<img src=x"));
    assert!(!html.contains("<h2><script>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("data-export="));
    assert!(html.contains("default-src 'none'"));
}

#[test]
fn glob_separator_and_canonicalization() {
    let f = Fixture::new(false);
    let mut c = Config::load(&f.config).unwrap();
    let w = &mut c.watches[0];
    w.branches.all = false;
    w.branches.include = vec!["release/*".into()];
    assert!(w.branch_matches("release/22"));
    assert!(!w.branch_matches("release/22/patch"));
    let mut other = w.clone();
    other.id = "different".into();
    other.recipients = vec!["bob".into()];
    other.remotes = vec!["upstream".into()];
    assert_eq!(w.selector_key(), other.selector_key());
}

#[test]
fn structural_engine_handles_formatting_and_fallback() {
    let c = git_monitor::config::DiffConfig::default();
    let limits = git_monitor::config::Limits::default();
    let a = b"int value = 1;\n";
    let b = b"int  value=1;\n";
    let d = git_monitor::diff::build("x.c", a, b, 1, 1, None, &c, &limits);
    assert_eq!(d.engine, "syntax");
    assert!(d.syntax.unwrap().text_only_change);
    let d = git_monitor::diff::build("x.unknown", a, b, 1, 1, None, &c, &limits);
    assert_eq!(d.engine, "text");
    assert_eq!(d.note, "unsupported_language");
}

#[test]
fn rendered_spans_preserve_source_lines_and_exact_exports() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    for (old, new) in [
        ("int x = 1;\n", "int x = 2;\n"),
        ("α = '🦀';\r\n", "α = '🦋';\r\n"),
        ("return 1;", "return 2;"),
        ("one\ntwo\nthree\n", "one\nchanged\nthree\n"),
    ] {
        let file = git_monitor::diff::build(
            "test.c",
            old.as_bytes(),
            new.as_bytes(),
            10,
            20,
            None,
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(
            STANDARD.decode(&file.old_bytes).unwrap().as_slice(),
            old.as_bytes()
        );
        assert_eq!(
            STANDARD.decode(&file.new_bytes).unwrap().as_slice(),
            new.as_bytes()
        );
        let a: Vec<_> = old
            .split_inclusive('\n')
            .map(|s| s.trim_end_matches('\n'))
            .collect();
        let b: Vec<_> = new
            .split_inclusive('\n')
            .map(|s| s.trim_end_matches('\n'))
            .collect();
        for row in &file.rows {
            if let Some(line) = row.old {
                assert_eq!(
                    row.left.iter().map(|s| s.text.as_str()).collect::<String>(),
                    a[line - 10]
                );
            }
            if let Some(line) = row.new {
                assert_eq!(
                    row.right
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<String>(),
                    b[line - 20]
                );
            }
        }
    }
}

#[test]
fn oversized_blob_is_explicit_and_other_files_survive() {
    let f = Fixture::new(false);
    f.ok();
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(&f.config, format!("{text}\n[limits]\nmax_blob_bytes=150\n")).unwrap();
    std::fs::write(f.work.join("huge.c"), "x".repeat(1000)).unwrap();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Budget");
    f.ok();
    assert!(f.report().contains("blob budget exceeded"));
    assert!(f.report().contains("test.c"));
}

#[test]
fn root_commit_and_merge_resolution_use_first_parent() {
    let f = Fixture::new(true);
    f.ok();
    let base = git(&f.work, &["rev-parse", "HEAD"]);
    git(&f.work, &["branch", "side", &base]);
    f.change(&BEFORE.replace("value > 0", "value > 1"), "Main change");
    f.ok();
    git(&f.work, &["switch", "-q", "side"]);
    std::fs::write(
        f.work.join("test.c"),
        BEFORE.replace("value > 0", "value > 2"),
    )
    .unwrap();
    git(&f.work, &["commit", "-qam", "Side change"]);
    git(&f.work, &["switch", "-q", "main"]);
    let o = Command::new("git")
        .arg("-C")
        .arg(&f.work)
        .args(["merge", "--no-commit", "side"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    std::fs::write(
        f.work.join("test.c"),
        BEFORE.replace("value > 0", "value > 3"),
    )
    .unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["commit", "-qm", "Merge resolution"]);
    git(&f.work, &["push", "-q", "origin", "main"]);
    f.ok();
    assert!(f.report().contains("Merge resolution"));
    assert!(!f.report().contains("PRIVATE_NEIGHBOR"));
}

#[test]
fn config_rejects_bad_ids_and_conflicting_selectors() {
    let f = Fixture::new(true);
    let mut c = Config::load(&f.config).unwrap();
    c.watches[0].paths = Some(vec!["*.c".into()]);
    assert!(c.validate().is_err());
}

#[test]
fn unrelated_root_history_is_analyzed_after_force_push() {
    let f = Fixture::new(false);
    f.ok();
    git(&f.work, &["switch", "--orphan", "replacement"]);
    std::fs::write(f.work.join("test.c"), "int replacement_root = 7;\n").unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["commit", "-qm", "Unrelated root"]);
    git(
        &f.work,
        &["push", "--force", "-q", "origin", "HEAD:refs/heads/main"],
    );
    f.ok();
    assert!(f.report().contains("Unrelated root"));
    assert!(!f.report().contains("analysis_failed"));
}

#[test]
fn repository_owner_rejects_another_state_directory() {
    let f = Fixture::new(false);
    f.ok();
    let text = std::fs::read_to_string(&f.config)
        .unwrap()
        .replace("state_dir='state'", "state_dir='other-state'");
    std::fs::write(&f.config, text).unwrap();
    let result = f.run();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("different state_dir"));
}

#[test]
fn mime_parts_fit_and_oversized_items_do_not_loop() {
    let f = Fixture::new(false);
    let mut cfg = Config::load(&f.config).unwrap();
    cfg.defaults.max_message_bytes = 40000;
    let mail = git_monitor::mail::MailConfig {
        version: 1,
        smtp: git_monitor::mail::Smtp {
            host: "localhost".into(),
            port: 2525,
            security: "none".into(),
            username: None,
            password: None,
            password_env: None,
            from: "Git Monitor <test@example.invalid>".into(),
            timeout: "1s".into(),
        },
    };
    let mut event = git_monitor::model::EventView {
        id: 1,
        repository: "project".into(),
        remote: "origin".into(),
        branch: "main".into(),
        kind: "branch_updated".into(),
        old: None,
        new: None,
        observed_at: 0,
        watches: vec!["runtime".into()],
        commits: vec![],
        outgoing: vec![],
        files: vec![],
        notes: vec![],
    };
    event.files.push(git_monitor::diff::build(
        "big.c",
        b"",
        &vec![b'a'; 500_000],
        1,
        1,
        None,
        &cfg.diff,
        &cfg.limits,
    ));
    let report = Report {
        recipient: "alice".into(),
        generated_at: 0,
        events: vec![event],
    };
    let parts = git_monitor::mail::package(&report, &cfg.recipients["alice"], &mail, &cfg).unwrap();
    assert!(parts.iter().all(|p| p.1.len() <= 40000));
    assert!(parts[0].2.contains("MIME item exceeds"));
}

#[test]
fn smtp_retry_reuses_saved_mime_and_message_id() {
    use std::io::{BufRead, BufReader, Write};
    let f = Fixture::new(false);
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Email change");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    let mail = f.root.join("mail.toml");
    std::fs::write(&mail,format!("version=1\n[smtp]\nhost='127.0.0.1'\nport={port}\nsecurity='none'\nfrom='Git Monitor <test@example.invalid>'\ntimeout='1s'\n")).unwrap();
    let run = || {
        Command::new(BIN)
            .args([
                "--config",
                f.config.to_str().unwrap(),
                "--mail-config",
                mail.to_str().unwrap(),
                "run",
                "--once",
                "--force",
            ])
            .output()
            .unwrap()
    };
    let o = run();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (id, mime): (String, Vec<u8>) = f
        .db()
        .query_row(
            "SELECT id,mime FROM deliveries WHERE state='uncertain'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE deliveries SET next_attempt=?",
            [git_monitor::now() + 10000],
        )
        .unwrap();
    let deferred = run();
    assert!(deferred.status.success());
    let count: i64 = f
        .db()
        .query_row("SELECT attempts FROM deliveries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "ordinary runs must respect SMTP retry backoff");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        socket.write_all(b"220 localhost SMTP test\r\n").unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut data = String::new();
        let mut in_data = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if in_data {
                if line == ".\r\n" {
                    in_data = false;
                    socket.write_all(b"250 accepted\r\n").unwrap();
                } else {
                    data.push_str(&line);
                }
                continue;
            }
            if line.starts_with("EHLO") {
                socket
                    .write_all(b"250-localhost\r\n250 8BITMIME\r\n")
                    .unwrap();
            } else if line.starts_with("DATA") {
                socket.write_all(b"354 send data\r\n").unwrap();
                in_data = true;
            } else if line.starts_with("QUIT") {
                socket.write_all(b"221 bye\r\n").unwrap();
                break;
            } else {
                socket.write_all(b"250 OK\r\n").unwrap();
            }
        }
        data
    });
    let o = Command::new(BIN)
        .args([
            "--config",
            f.config.to_str().unwrap(),
            "--mail-config",
            mail.to_str().unwrap(),
            "retry-deliveries",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let received = server.join().unwrap();
    assert!(received.contains(&id));
    assert!(received.contains("Content-Disposition: attachment"));
    let (saved, attempts, state): (Vec<u8>, i64, String) = f
        .db()
        .query_row(
            "SELECT mime,attempts,state FROM deliveries WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(saved, mime);
    assert_eq!(attempts, 2);
    assert_eq!(state, "sent");
}

#[cfg(unix)]
#[test]
fn sigterm_drains_then_terminates_fetch_process_group() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(false);
    f.ok();
    let bin = f.root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = f.root.join("fetch-started");
    let actual = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let wrapper = bin.join("git");
    std::fs::write(&wrapper,format!("#!/bin/sh\ncase \" $* \" in *' fetch '*) touch '{}'; sleep 30;; esac\nexec '{}' \"$@\"\n",marker.display(),actual.trim())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new(BIN)
        .args([
            "--config",
            f.config.to_str().unwrap(),
            "run",
            "--once",
            "--force",
        ])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !marker.exists() {
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(3) {
            let _ = child.kill();
            panic!("shutdown exceeded deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(f.count("attempts"), 1);
    f.ok();
    assert_eq!(f.count("attempts"), 0);
    assert_eq!(f.count("events"), 0);
}
