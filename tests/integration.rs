use git_monitor::{
    config::Config,
    git::{repository_path, Git},
    model::Report,
    process::Shutdown,
    store::{source_key, Attempt, FetchPlan, Store},
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
        git_monitor::process::set_worker_executable(PathBuf::from(BIN));
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
    fn branches(&self, branches: &str) {
        let text = std::fs::read_to_string(&self.config).unwrap();
        std::fs::write(&self.config, text.replace("branches={all=true}", branches)).unwrap();
    }
    fn begin_attempt(&self) -> (Store, i64, Attempt, Git) {
        let cfg = Config::load(&self.config).unwrap();
        let store = Store::open(&cfg.storage.state_dir).unwrap();
        let key = source_key("project", "origin", self.remote.to_str().unwrap());
        let (old, previous, _) = store.snapshot(&key).unwrap().unwrap();
        let git = Git {
            path: repository_path(&cfg.storage.repositories_dir, "project"),
            execution: cfg.execution.clone(),
            limits: cfg.limits.clone(),
            shutdown: Shutdown::default(),
        };
        let attempt = Attempt {
            key,
            repository: "project".into(),
            remote: "origin".into(),
            path: git.path.clone(),
            old,
            published: git.refs("origin").unwrap(),
            initialized: true,
            plan: FetchPlan::for_watches(&cfg.watches),
            watches: cfg.watches,
            previous,
            execution: cfg.execution,
            limits: cfg.limits,
            diff: cfg.diff,
        };
        for oid in attempt.old.values() {
            git.pin(oid).unwrap();
        }
        let id = store.begin(&attempt).unwrap();
        (store, id, attempt, git)
    }
    #[cfg(unix)]
    fn wrapped_run(&self, script: &str) -> Output {
        use std::os::unix::fs::PermissionsExt;
        let actual = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let bin = self.root.join("wrapper-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let wrapper = bin.join("git");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n{}\nexec '{}' \"$@\"\n",
                script.replace("@GIT@", actual.trim()),
                actual.trim()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        Command::new(BIN)
            .args([
                "--config",
                self.config.to_str().unwrap(),
                "run",
                "--once",
                "--force",
            ])
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    }
}

#[cfg(unix)]
#[test]
fn narrow_discovery_failure_preserves_snapshot_and_noop_skips_download() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    let before: String = f
        .db()
        .query_row("SELECT refs FROM snapshots", [], |r| r.get(0))
        .unwrap();
    let failed = f.wrapped_run(
        "case \" $* \" in *' ls-remote '*) echo 'sensitive transport error' >&2; exit 128;; esac",
    );
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("sensitive transport error"));
    assert_eq!(f.count("events"), 0);
    let after: String = f
        .db()
        .query_row("SELECT refs FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    let noop = f.wrapped_run("case \" $* \" in *' fetch '*) exit 128;; esac");
    assert!(
        noop.status.success(),
        "{}",
        String::from_utf8_lossy(&noop.stderr)
    );
    assert_eq!(f.count("attempts"), 0);
    // An unrestricted watch uses direct fetch and needs no ls-remote request.
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(
        &f.config,
        text.replace("branches={include=['main']}", "branches={all=true}"),
    )
    .unwrap();
    let full = f.wrapped_run("case \" $* \" in *' ls-remote '*) exit 128;; esac");
    assert!(
        full.status.success(),
        "{}",
        String::from_utf8_lossy(&full.stderr)
    );
}

#[cfg(unix)]
#[test]
fn disappearing_branch_between_discovery_and_download_is_rediscovered() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Racing update");
    let once = f.root.join("discovered-once");
    let count = f.root.join("discovery-count");
    let o = f.wrapped_run(&format!(
        "case \" $* \" in *' ls-remote '*)\n echo observed >> '{}'\n if [ ! -e '{}' ]; then\n '@GIT@' \"$@\" || exit $?\n touch '{}'\n '@GIT@' -C '{}' update-ref -d refs/heads/main\n exit $?\n fi;; esac",
        count.display(), once.display(), once.display(), f.remote.display()));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 2);
    assert_eq!(f.count("events"), 1);
    let event: String = f
        .db()
        .query_row("SELECT body FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<git_monitor::model::EventView>(&event)
            .unwrap()
            .kind,
        "branch_deleted"
    );
    let snapshot: String = f
        .db()
        .query_row("SELECT refs FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(snapshot, "{}");
    assert_eq!(f.count("attempts"), 0);
}

#[cfg(unix)]
#[test]
fn force_push_during_download_rechecks_the_discovered_oid() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Unpublished racing tip",
    );
    git(&f.work, &["checkout", "--orphan", "replacement"]);
    git(&f.work, &["rm", "-rf", "."]);
    std::fs::write(f.work.join("test.c"), "int replacement = 7;\n").unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["commit", "-qm", "Raced replacement root"]);
    let replacement = git(&f.work, &["rev-parse", "HEAD"]);
    git(&f.work, &["push", "-q", "origin", "replacement"]);
    git(&f.remote, &["update-ref", "-d", "refs/heads/replacement"]);
    let once = f.root.join("discovered-once");
    let count = f.root.join("discovery-count");
    let o = f.wrapped_run(&format!(
        "case \" $* \" in *' ls-remote '*)\n echo observed >> '{}'\n if [ ! -e '{}' ]; then\n '@GIT@' \"$@\" || exit $?\n touch '{}'\n '@GIT@' -C '{}' update-ref refs/heads/main '{}'\n exit $?\n fi;; esac",
        count.display(), once.display(), once.display(), f.remote.display(), replacement));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 2);
    assert_eq!(f.count("events"), 1);
    let event: String = f
        .db()
        .query_row("SELECT body FROM events", [], |r| r.get(0))
        .unwrap();
    let event: git_monitor::model::EventView = serde_json::from_str(&event).unwrap();
    assert_eq!(event.kind, "history_rewritten");
    assert_eq!(event.new.as_deref(), Some(replacement.as_str()));
    assert!(f.report().contains("Raced replacement root"));
    assert!(!f.report().contains("Unpublished racing tip"));
}

#[test]
fn narrow_plan_unions_independent_watches() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    let text = std::fs::read_to_string(&f.config).unwrap();
    let extra = text
        .split("[[watches]]")
        .nth(1)
        .unwrap()
        .replace("id='runtime'", "id='release'")
        .replace(
            "branches={include=['main']}",
            "branches={include=['release/**']}",
        );
    std::fs::write(&f.config, format!("{text}\n[[watches]]{extra}")).unwrap();
    git(
        &f.work,
        &[
            "push",
            "-q",
            "origin",
            "main:refs/heads/release/2",
            "main:refs/heads/ignored",
        ],
    );
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Both scopes");
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/2"],
    );
    f.ok();
    assert_eq!(f.count("events"), 2);
    assert_eq!(f.count("matches"), 2);
    let snapshot: String = f
        .db()
        .query_row("SELECT refs FROM snapshots", [], |r| r.get(0))
        .unwrap();
    let refs: std::collections::BTreeMap<String, String> = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(
        refs.keys().cloned().collect::<Vec<_>>(),
        ["main", "release/2"]
    );
}

#[test]
fn narrow_publication_checks_old_refs_without_partial_changes() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Transaction conflict",
    );
    let (store, id, attempt, backend) = f.begin_attempt();
    let desired = backend.discover("origin", false).unwrap();
    backend
        .download_selected("origin", &attempt.staging_prefix(id), &desired, false)
        .unwrap();
    let old_marker = backend.ref_oid(&attempt.marker_ref()).unwrap().unwrap();
    let marker = backend
        .write_manifest(b"conflicting transaction".to_vec())
        .unwrap();
    // Simulate another writer after the expected old state was recorded.
    backend
        .raw(&["update-ref", "refs/remotes/origin/main", &desired["main"]])
        .unwrap();
    assert!(backend
        .publish_selected(
            "origin",
            &attempt.published,
            &desired,
            &attempt.marker_ref(),
            &marker,
            Some(&old_marker)
        )
        .is_err());
    assert_eq!(
        backend.ref_oid(&attempt.marker_ref()).unwrap().as_deref(),
        Some(old_marker.as_str())
    );
    assert_eq!(backend.refs("origin").unwrap(), desired);
    assert_eq!(f.count("events"), 0);
    drop(store);
}

#[test]
fn narrow_fetch_observes_only_selected_heads_and_objects() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main', 'release/**'],exclude=['release/private/**']}");
    git(&f.work, &["checkout", "--orphan", "secret"]);
    git(&f.work, &["rm", "-rf", "."]);
    std::fs::write(f.work.join("secret.c"), "int PRIVATE_SOURCE = 42;\n").unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["commit", "-qm", "Unwatched root"]);
    let secret = git(&f.work, &["rev-parse", "HEAD"]);
    git(
        &f.work,
        &[
            "push",
            "-q",
            "origin",
            "secret",
            "secret:refs/heads/release/private/test",
        ],
    );
    git(&f.work, &["checkout", "main"]);
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/1"],
    );
    f.ok();
    let (store, id, attempt, backend) = f.begin_attempt();
    assert_eq!(
        backend
            .refs("origin")
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["main", "release/1"]
    );
    assert!(
        backend.raw(&["cat-file", "-e", &secret]).is_err(),
        "unselected orphan objects must not be downloaded"
    );
    assert_eq!(attempt.old.len(), 2);
    // A no-op observation still has a durable marker, without fetching again.
    let mut attempt = attempt;
    let refs = git_monitor::monitor::fetch_attempt(&store, id, &mut attempt, &backend).unwrap();
    assert_eq!(refs, attempt.old);
    drop(store);
    f.ok();
    assert_eq!(f.count("events"), 0);
    assert_eq!(
        git(
            &backend.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/git-monitor/fetch/"
            ]
        ),
        ""
    );
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Selected main update",
    );
    f.ok();
    assert_eq!(f.count("events"), 1);
    assert!(f.report().contains("Selected main update"));
}

#[test]
fn narrow_fetch_records_creation_and_deletion_of_final_branch() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main', 'release/**']}");
    f.ok();
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/2"],
    );
    f.ok();
    assert_eq!(f.count("events"), 1);
    git(&f.remote, &["update-ref", "-d", "refs/heads/main"]);
    git(&f.remote, &["update-ref", "-d", "refs/heads/release/2"]);
    f.ok();
    assert_eq!(f.count("events"), 3);
    let snapshots: String = f
        .db()
        .query_row("SELECT refs FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(snapshots, "{}");
    f.ok();
    assert_eq!(f.count("events"), 3);
    assert_eq!(f.count("attempts"), 0);
}

#[test]
fn changing_branch_coverage_establishes_baselines_without_false_events() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/2"],
    );
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Main pending");
    // Expand from narrow to all: the already existing release ref is a baseline.
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(
        &f.config,
        text.replace("branches={include=['main']}", "branches={all=true}"),
    )
    .unwrap();
    f.ok();
    assert_eq!(f.count("events"), 1);
    assert_eq!(f.count("matches"), 1);
    // Narrowing all back to main must not invent a release branch deletion.
    f.branches("branches={include=['main']}");
    f.ok();
    assert_eq!(f.count("events"), 1);
    assert!(f.report().contains("Main pending"));
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(
        &f.config,
        text.replace(
            "branches={include=['main']}",
            "branches={include=['main','release/**']}",
        ),
    )
    .unwrap();
    f.ok();
    assert_eq!(f.count("events"), 1);
    git(
        &f.work,
        &["push", "-q", "origin", "main:refs/heads/release/2"],
    );
    f.ok();
    assert_eq!(f.count("events"), 2);
}

#[test]
fn narrow_publication_is_recovered_before_remote_rollback() {
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    let a = git(&f.work, &["rev-parse", "HEAD"]);
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Narrow before crash",
    );
    let (store, id, mut attempt, backend) = f.begin_attempt();
    git_monitor::monitor::fetch_attempt(&store, id, &mut attempt, &backend).unwrap();
    drop(store); // Simulated crash after publication, before the SQLite transaction.
    git::rollback(&f.work, &a);
    f.ok();
    assert_eq!(f.count("events"), 2);
    let events: Vec<String> = f
        .db()
        .prepare("SELECT body FROM events ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let events: Vec<git_monitor::model::EventView> = events
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(events[0].old.as_deref(), Some(a.as_str()));
    assert_eq!(events[1].new.as_deref(), Some(a.as_str()));
    assert_eq!(events[0].new, events[1].old);
    assert_eq!(f.count("attempts"), 0);
}

#[test]
fn unfinished_narrow_download_never_becomes_an_observation() {
    use git_monitor::store::NarrowObservation;
    let f = Fixture::new(false);
    f.branches("branches={include=['main']}");
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Unpublished download",
    );
    let (mut store, id, mut attempt, backend) = f.begin_attempt();
    let desired = backend.discover("origin", false).unwrap();
    let previous_marker = backend.ref_oid(&attempt.marker_ref()).unwrap();
    let marker = backend
        .write_manifest(b"unpublished test manifest".to_vec())
        .unwrap();
    attempt.plan = FetchPlan::Selected {
        observation: Some(NarrowObservation {
            refs: desired.clone(),
            marker,
            previous_marker,
        }),
    };
    store.update_attempt(id, &attempt).unwrap();
    assert!(backend
        .download_selected("origin", &attempt.staging_prefix(id), &desired, false)
        .unwrap());
    assert!(!git(
        &backend.path,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &attempt.staging_prefix(id)
        ]
    )
    .is_empty());
    assert_eq!(backend.refs("origin").unwrap(), attempt.published);
    let before = store.snapshot(&attempt.key).unwrap();
    assert_eq!(
        git_monitor::monitor::recover(&mut store, &Shutdown::default()).unwrap(),
        0
    );
    assert_eq!(store.snapshot(&attempt.key).unwrap(), before);
    assert_eq!(f.count("events"), 0);
    assert_eq!(
        git(
            &backend.path,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &attempt.staging_prefix(id)
            ]
        ),
        ""
    );
    drop(store);
    f.ok();
    assert_eq!(f.count("events"), 1);
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
        plan: FetchPlan::All,
    };
    let id = store.begin(&attempt).unwrap();
    // A pending attempt saved by schema 1 has no fetch-plan field.
    let mut legacy = serde_json::to_value(&attempt).unwrap();
    legacy.as_object_mut().unwrap().remove("plan");
    store
        .db
        .execute(
            "UPDATE attempts SET context=? WHERE id=?",
            rusqlite::params![legacy.to_string(), id],
        )
        .unwrap();
    store.db.execute_batch("PRAGMA user_version=1;").unwrap();
    for oid in attempt.old.values() {
        git.pin(oid).unwrap();
    }
    git.fetch("origin", false).unwrap();
    drop(store);
    git::rollback(&f.work, &a);
    f.ok();
    assert_eq!(f.count("events"), 2);
    assert_eq!(f.count("attempts"), 0);
    assert_eq!(
        f.db()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
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
fn continuously_selected_paths_survive_filter_changes() {
    let original = "paths = ['**/*.c', '*.c']";
    for (before, after) in [
        (original, "paths = ['test.c']"),
        ("", "paths = ['test.c']"),
        (original, "paths = ['*.c']\nexclude_paths = ['excluded.c']"),
        ("paths = ['test.c']", "paths = ['*.c']"),
        ("paths = ['test.c']", ""),
    ] {
        let f = Fixture::new(false);
        let text = std::fs::read_to_string(&f.config).unwrap();
        let baseline = text.replace(original, before);
        std::fs::write(&f.config, &baseline).unwrap();
        f.ok();
        std::fs::write(f.work.join("excluded.c"), "int excluded = 1;\n").unwrap();
        f.change(
            &BEFORE.replace("value > 0", "value >= 0"),
            "Continuous change",
        );
        std::fs::write(&f.config, text.replace(original, after)).unwrap();
        // Persist the transition before worker execution (also exercises restart).
        let cfg = Config::load(&f.config).unwrap();
        let mut store = Store::open(&cfg.storage.state_dir).unwrap();
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true).unwrap();
        let jobs = store.jobs().unwrap();
        assert_eq!(jobs.len(), 1, "{before:?} -> {after:?}");
        assert!(jobs[0].1.previous_watch.is_some());
        drop(store);
        f.ok();
        assert!(f.report().contains("Continuous change"));
        let body: String = f
            .db()
            .query_row("SELECT body FROM matches", [], |r| r.get(0))
            .unwrap();
        let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
        let paths: Vec<_> = event
            .commits
            .iter()
            .flat_map(|c| &c.files)
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(paths, ["test.c"], "{before:?} -> {after:?}");
        f.ok();
        assert_eq!(f.count("matches"), 1, "no replay on next poll");
    }
}

#[test]
fn expanded_scope_is_baselined_and_cache_respects_previous_scope() {
    let f = Fixture::new(false);
    let text = std::fs::read_to_string(&f.config).unwrap();
    let second = text
        .split("[[watches]]")
        .nth(1)
        .unwrap()
        .replace("id='runtime'", "id='rust'")
        .replace("paths = ['**/*.c', '*.c']", "paths = ['*.rs']");
    let text = format!("{text}\n[[watches]]{second}");
    std::fs::write(&f.config, &text).unwrap();
    f.ok();
    std::fs::write(f.work.join("new.rs"), "fn added() {}\n").unwrap();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Both languages");
    let expanded = text
        .replace("paths = ['**/*.c', '*.c']", "")
        .replace("paths = ['*.rs']", "");
    std::fs::write(&f.config, expanded).unwrap();
    let output = f.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("analyses=2"));
    let db = f.db();
    for (watch, expected) in [("runtime", "test.c"), ("rust", "new.rs")] {
        let body: String = db
            .query_row("SELECT body FROM matches WHERE watch_id=?", [watch], |r| {
                r.get(0)
            })
            .unwrap();
        let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
        let paths: Vec<_> = event
            .commits
            .iter()
            .flat_map(|c| &c.files)
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(paths, [expected]);
    }
    std::fs::write(
        f.work.join("new.rs"),
        "fn added() { println!(\"now tracked\"); }\n",
    )
    .unwrap();
    f.change(&BEFORE.replace("value > 0", "value > 2"), "After baseline");
    let output = f.run();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("analyses=1"));
    let body: String = db
        .query_row(
            "SELECT body FROM matches WHERE watch_id='runtime' ORDER BY event_id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
    assert_eq!(event.commits[0].files.len(), 2);
    // Pre-upgrade tasks still deserialize without the new optional restriction.
    let task: String = db
        .query_row("SELECT task FROM jobs LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let mut task: serde_json::Value = serde_json::from_str(&task).unwrap();
    task.as_object_mut().unwrap().remove("previous_watch");
    assert!(serde_json::from_value::<git_monitor::store::Task>(task)
        .unwrap()
        .previous_watch
        .is_none());
}

#[test]
fn revoked_subscriptions_do_not_consume_the_pending_report_budget() {
    for remove_watch in [false, true] {
        let f = Fixture::new(false);
        let text = std::fs::read_to_string(&f.config).unwrap();
        let extra = text
            .split("[[watches]]")
            .nth(1)
            .unwrap()
            .replace("id='runtime'", "id='second'");
        let text = text.replace(
            "[recipients.alice]",
            "[recipients.bob]\nto=['bob@example.invalid']\n[recipients.alice]",
        );
        std::fs::write(&f.config, format!("{text}\n[[watches]]{extra}")).unwrap();
        f.ok();
        f.change(
            &BEFORE.replace("value > 0", "value >= 0"),
            "Still owed to Alice",
        );
        f.ok();
        let mut cfg = Config::load(&f.config).unwrap();
        if remove_watch {
            cfg.watches.remove(0);
        } else {
            cfg.watches[0].recipients = vec!["bob".into()];
        }
        let len: usize = f
            .db()
            .query_row(
                "SELECT length(body) FROM matches ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        cfg.limits.max_job_output_bytes = len + 100;
        let store = Store::open(&cfg.storage.state_dir).unwrap();
        let (report, ids) = git_monitor::monitor::report(&cfg, &store, "alice", true).unwrap();
        assert_eq!(
            report.events.len(),
            1,
            "eligible row must fit in this same call"
        );
        assert_eq!(report.events[0].watches, ["second"]);
        assert_eq!(ids.len(), 1);
        let revoked: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM matches WHERE digest='revoked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(revoked, 1);
        store
            .db
            .execute(
                "UPDATE matches SET digest='test-delivered' WHERE id=?",
                [ids[0]],
            )
            .unwrap();
        assert!(git_monitor::monitor::report(&cfg, &store, "alice", true)
            .unwrap()
            .0
            .events
            .is_empty());
    }
}

#[test]
fn cli_retries_failed_analysis_and_exports_the_recovered_report() {
    let f = Fixture::new(false);
    let cli = |args: &[&str]| {
        Command::new(BIN)
            .arg("--config")
            .arg(&f.config)
            .args(args)
            .output()
            .unwrap()
    };
    for action in ["check-config", "doctor"] {
        assert!(cli(&[action]).status.success());
    }
    f.ok();
    let original = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(
        &f.config,
        format!("{original}\n[limits]\nmax_job_output_bytes=100\n"),
    )
    .unwrap();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Recovered analysis",
    );
    f.ok();
    let (id, state): (i64, String) = f
        .db()
        .query_row("SELECT id,state FROM jobs", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(f.count("cache"), 0, "failures cannot poison shared cache");
    let status = cli(&["status"]);
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("failed jobs  1"));
    assert!(!cli(&["retry-analysis", "--job", "99999"]).status.success());
    std::fs::write(&f.config, original).unwrap();
    assert!(cli(&["retry-analysis", "--job", &id.to_string()])
        .status
        .success());
    let output = f.root.join("exports");
    assert!(cli(&[
        "run",
        "--once",
        "--force",
        "--output-dir",
        output.to_str().unwrap()
    ])
    .status
    .success());
    assert!(std::fs::read_to_string(output.join("alice.html"))
        .unwrap()
        .contains("Recovered analysis"));
    let preview = f.root.join("preview.html");
    assert!(cli(&[
        "preview",
        "--recipient",
        "alice",
        "--output",
        preview.to_str().unwrap()
    ])
    .status
    .success());
    assert!(std::fs::read_to_string(preview)
        .unwrap()
        .contains("Recovered analysis"));
    let state: String = f
        .db()
        .query_row("SELECT state FROM jobs WHERE id=?", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "done");
}

#[test]
fn ordinary_poll_respects_interval_and_failure_backoff() {
    let f = Fixture::new(false);
    f.ok();
    let mut cfg = Config::load(&f.config).unwrap();
    cfg.defaults.poll_every = "1h".into();
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    let key = source_key("project", "origin", f.remote.to_str().unwrap());
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "After interval");
    store
        .db
        .execute(
            "UPDATE remote_status SET next_poll=?",
            [git_monitor::now() + 100],
        )
        .unwrap();
    assert_eq!(
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), false)
            .unwrap()
            .0,
        0
    );
    store
        .db
        .execute("UPDATE remote_status SET next_poll=0", [])
        .unwrap();
    assert_eq!(
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), false)
            .unwrap()
            .0,
        0
    );
    store.fetch_error(&key).unwrap();
    assert!(store.next_poll(&key).unwrap() > git_monitor::now());
    assert!(store.status().unwrap().contains("failures=1"));
    store
        .db
        .execute("UPDATE snapshots SET checked=0", [])
        .unwrap();
    store
        .db
        .execute("UPDATE remote_status SET next_poll=0", [])
        .unwrap();
    assert_eq!(
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), false)
            .unwrap()
            .0,
        1
    );
    assert!(store.status().unwrap().contains("failures=0"));
}

#[test]
fn report_budget_preserves_later_pending_matches_and_marks_oversized_items() {
    let f = Fixture::new(false);
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "First pending");
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 1"), "Second pending");
    f.ok();
    let mut cfg = Config::load(&f.config).unwrap();
    let len: usize = f
        .db()
        .query_row(
            "SELECT length(body) FROM matches ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    cfg.limits.max_job_output_bytes = len + 1;
    let store = Store::open(&cfg.storage.state_dir).unwrap();
    let (report, ids) = git_monitor::monitor::report(&cfg, &store, "alice", true).unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(report.events[0].commits[0].subject, "First pending");
    assert!(report.events[0]
        .notes
        .iter()
        .any(|n| n.contains("window limited")));
    store
        .db
        .execute(
            "UPDATE matches SET digest='test-delivered' WHERE id=?",
            [ids[0]],
        )
        .unwrap();
    cfg.limits.max_job_output_bytes = 1;
    let (report, ids) = git_monitor::monitor::report(&cfg, &store, "alice", true).unwrap();
    assert_eq!(ids.len(), 1);
    assert!(report.events[0]
        .notes
        .iter()
        .any(|n| n.contains("oversized_item")));
    assert!(report.events[0].commits.is_empty());
    assert_eq!(report.events[0].watches, ["runtime"]);
    cfg.limits.max_job_output_bytes = 128_000_000;
    let (report, _) = git_monitor::monitor::report(&cfg, &store, "alice", true).unwrap();
    assert_eq!(report.events[0].commits[0].subject, "Second pending");
}

fn offline_mail() -> git_monitor::mail::MailConfig {
    git_monitor::mail::MailConfig {
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
    }
}

#[test]
fn prepared_outbox_is_rebuilt_for_changed_addresses() {
    let f = Fixture::new(false);
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Queued digest");
    f.ok();
    let mut cfg = Config::load(&f.config).unwrap();
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    let mail = offline_mail();
    let shutdown = Shutdown::default();
    assert_eq!(
        git_monitor::monitor::outputs(&cfg, &mut store, Some(&mail), &shutdown, None).unwrap(),
        1
    );
    cfg.recipients.get_mut("alice").unwrap().to = vec!["new@example.invalid".into()];
    assert_eq!(
        git_monitor::monitor::outputs(&cfg, &mut store, Some(&mail), &shutdown, None).unwrap(),
        1
    );
    let revoked: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM deliveries WHERE state='revoked'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revoked, 1);
    let mime: Vec<u8> = store
        .db
        .query_row(
            "SELECT mime FROM deliveries WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mime = String::from_utf8(mime).unwrap();
    assert!(mime.contains("To: new@example.invalid"));
    assert!(!mime.contains("To: alice@example.invalid"));
}

#[test]
fn binary_mode_and_gitlink_changes_have_explicit_kinds() {
    let f = Fixture::new(false);
    let text = std::fs::read_to_string(&f.config)
        .unwrap()
        .replace("paths = ['**/*.c', '*.c']", "");
    std::fs::write(&f.config, text).unwrap();
    f.ok();
    std::fs::write(f.work.join("binary.dat"), b"binary\0data").unwrap();
    git(&f.work, &["add", "."]);
    git(&f.work, &["update-index", "--chmod=+x", "test.c"]);
    let oid = git(&f.work, &["rev-parse", "HEAD"]);
    git(
        &f.work,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},submodule"),
        ],
    );
    git(&f.work, &["commit", "-qm", "Metadata changes"]);
    git(&f.work, &["push", "-q", "origin", "main"]);
    f.ok();
    let body: String = f
        .db()
        .query_row("SELECT body FROM matches", [], |r| r.get(0))
        .unwrap();
    let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
    let kinds: std::collections::BTreeMap<_, _> = event.commits[0]
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.kind.as_str()))
        .collect();
    assert_eq!(kinds["binary.dat"], "binary");
    assert_eq!(kinds["test.c"], "mode_changed");
    assert_eq!(kinds["submodule"], "gitlink");
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
fn profile_identity_covers_versions_and_allowed_driver_rules() {
    let p = git_monitor::profile::AnalysisProfile::freeze(&Default::default()).unwrap();
    assert!(!p.git.is_empty());
    for field in ["git", "analyzer", "dependencies", "drivers", "model"] {
        let mut next = p.clone();
        match field {
            "git" => next.git.push_str(" changed"),
            "analyzer" => next.analyzer.push('1'),
            "dependencies" => next.dependencies.push('1'),
            "drivers" => {
                next.drivers.insert("custom".into(), "^FUNC".into());
            }
            _ => next.model += 1,
        }
        assert_ne!(p.key(), next.key(), "{field}");
    }
}

#[test]
fn doctor_checks_existing_queue_integrity_without_network() {
    let f = Fixture::new(false);
    f.ok();
    let check = || {
        Command::new(BIN)
            .arg("--config")
            .arg(&f.config)
            .arg("doctor")
            .output()
            .unwrap()
    };
    let output = check();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("queue integrity OK"));
    f.db()
        .execute(
            "INSERT INTO jobs(event_id,watch_id,task) VALUES(999999,'orphan','{}')",
            [],
        )
        .unwrap();
    let output = check();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("orphaned analysis jobs"));
}

#[test]
fn global_function_rules_do_not_override_frozen_analysis() {
    let f = Fixture::new(true);
    std::fs::write(f.work.join(".gitattributes"), "*.c diff=cpp\n").unwrap();
    f.change(BEFORE, "Set built-in function rules");
    f.ok();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Frozen rules");
    let global = f.root.join("host.gitconfig");
    std::fs::write(&global, "[diff \"cpp\"]\n  xfuncname = ^NOT_A_FUNCTION$\n").unwrap();
    let output = Command::new(BIN)
        .arg("--config")
        .arg(&f.config)
        .args(["run", "--once", "--force"])
        .env("GIT_CONFIG_GLOBAL", &global)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(f.report().contains("Frozen rules"));
    assert!(!f.report().contains("analysis_failed"));
}

#[test]
fn configured_file_context_is_respected() {
    let f = Fixture::new(false);
    f.ok();
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(&f.config, format!("{text}\n[diff]\ncontext_lines=0\n")).unwrap();
    f.change(&BEFORE.replace("value > 0", "value >= 0"), "Zero context");
    f.ok();
    let body: String = f
        .db()
        .query_row("SELECT body FROM matches", [], |r| r.get(0))
        .unwrap();
    let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
    assert!(!event.commits[0].files[0]
        .rows
        .iter()
        .any(|r| r.kind == "context"));
}

#[test]
fn function_document_has_exact_original_scope_and_no_whole_file_patch() {
    let f = Fixture::new(true);
    let text = std::fs::read_to_string(&f.config)
        .unwrap()
        .replace("match = 'target'", "match = 'PRIVATE_NEIGHBOR'");
    std::fs::write(&f.config, text).unwrap();
    f.ok();
    f.change(
        &BEFORE.replace("return 900", "return 901"),
        "Second function scope",
    );
    f.ok();
    let body: String = f
        .db()
        .query_row("SELECT body FROM matches", [], |r| r.get(0))
        .unwrap();
    let event: git_monitor::model::EventView = serde_json::from_str(&body).unwrap();
    let file = &event.commits[0].files[0];
    file.validate().unwrap();
    let doc = file.document.as_ref().unwrap();
    assert!(doc.old.original_range.start > 0);
    assert!(doc.old.start_line > 1);
    assert!(doc.old.blob.is_some());
    assert!(file.canonical_patch.is_none());
}

#[test]
fn maintenance_keeps_pending_results_and_gc_preserves_queued_objects() {
    let f = Fixture::new(false);
    f.ok();
    let base = git(&f.work, &["rev-parse", "HEAD"]);
    f.change(
        &BEFORE.replace("value > 0", "value > 1"),
        "Queued before rollback",
    );
    let cfg = Config::load(&f.config).unwrap();
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true).unwrap();
    git(&f.work, &["reset", "--hard", &base]);
    std::fs::write(
        f.work.join("test.c"),
        BEFORE.replace("value > 0", "value > 2"),
    )
    .unwrap();
    git(&f.work, &["commit", "-qam", "Replacement"]);
    git(&f.work, &["push", "--force", "-q", "origin", "main"]);
    git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true).unwrap();
    let summary =
        git_monitor::maintenance::run(&cfg, &mut store, &Shutdown::default(), true).unwrap();
    assert_eq!(summary.events, 0);
    assert_eq!(store.jobs().unwrap().len(), 2);
    drop(store);
    f.ok();
    assert!(f.report().contains("Queued before rollback"));
    assert!(!f.report().contains("analysis_failed"));
}

#[test]
fn maintenance_expires_completed_history_and_cleans_only_unbound_artifacts() {
    let f = Fixture::new(false);
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value > 1"),
        "Retained pending result",
    );
    f.ok();
    let mut cfg = Config::load(&f.config).unwrap();
    cfg.maintenance.history_days = 0;
    cfg.maintenance.artifact_days = 0;
    cfg.maintenance.temporary_hours = 0;
    cfg.maintenance.cache_items = 0;
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    store
        .db
        .execute(
            "UPDATE events SET body=json_set(body,'$.observed_at',0)",
            [],
        )
        .unwrap();
    assert_eq!(
        git_monitor::maintenance::run(&cfg, &mut store, &Shutdown::default(), false)
            .unwrap()
            .events,
        0
    );
    store
        .db
        .execute("UPDATE matches SET digest='delivered-test'", [])
        .unwrap();
    store.db.execute("INSERT INTO deliveries(id,recipient,revision,digest,mime,state) VALUES('pending-test','alice','test','protected','mime','uncertain')",[]).unwrap();
    store.db.execute("INSERT INTO deliveries(id,recipient,revision,digest,mime,state) VALUES('sent-test','alice','test','expired','mime','sent')",[]).unwrap();
    store
        .db
        .execute("INSERT INTO delivery_times VALUES('expired',0)", [])
        .unwrap();
    let artifacts = cfg.storage.state_dir.join("artifacts");
    std::fs::write(artifacts.join("orphan.html"), "orphan").unwrap();
    std::fs::write(artifacts.join("protected-1.html"), "pending attachment").unwrap();
    std::fs::write(cfg.storage.state_dir.join("tmp/orphan.tmp"), "orphan").unwrap();
    let result =
        git_monitor::maintenance::run(&cfg, &mut store, &Shutdown::default(), true).unwrap();
    assert_eq!(result.events, 1);
    assert!(result.files >= 2);
    assert!(result.pins > 0);
    assert!(artifacts.join("alice-latest.html").exists());
    assert!(artifacts.join("protected-1.html").exists());
    assert!(!artifacts.join("orphan.html").exists());
    assert_eq!(f.count("cache"), 0);
    assert_eq!(f.count("jobs"), 0);
    assert_eq!(f.count("deliveries"), 1);
}

#[test]
fn report_worker_budget_creates_a_diagnostic_without_blocking_the_run() {
    let f = Fixture::new(false);
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Saved scoped analysis",
    );
    let text = std::fs::read_to_string(&f.config).unwrap();
    std::fs::write(
        &f.config,
        format!("{text}\n[limits]\nmax_render_output_bytes=1\n"),
    )
    .unwrap();
    f.ok();
    assert!(f.report().contains("rendering/packaging exceeded"));
    let count: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM matches WHERE body LIKE '%Saved scoped analysis%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "render failure cannot discard full analysis");
    std::fs::write(&f.config, text).unwrap();
    f.ok();
    assert!(f.report().contains("Saved scoped analysis"));
}

#[test]
fn source_context_rotation_and_return_create_fresh_epochs() {
    let f = Fixture::new(false);
    f.ok();
    let mut cfg = Config::load(&f.config).unwrap();
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    let remote = cfg.repositories["project"].remotes["origin"].clone();
    let identity = git_monitor::store::source_identity("project", "origin", &remote);
    let a = store.active_source("project", "origin", &identity).unwrap();
    cfg.repositories
        .get_mut("project")
        .unwrap()
        .remotes
        .get_mut("origin")
        .unwrap()
        .access_context = "restricted".into();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Context change baseline",
    );
    assert_eq!(
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true)
            .unwrap()
            .0,
        0
    );
    cfg.repositories
        .get_mut("project")
        .unwrap()
        .remotes
        .get_mut("origin")
        .unwrap()
        .access_context
        .clear();
    assert_eq!(
        git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true)
            .unwrap()
            .0,
        0
    );
    assert_ne!(
        store.active_source("project", "origin", &identity).unwrap(),
        a
    );
}

#[test]
fn sha256_git_cache_matches_the_configured_object_format() {
    let f = Fixture::new(false);
    let source = f.root.join("sha256.git");
    git(
        &f.root,
        &[
            "init",
            "--bare",
            "--object-format=sha256",
            "-q",
            source.to_str().unwrap(),
        ],
    );
    git(&source, &["config", "user.name", "Test"]);
    git(&source, &["config", "user.email", "test@example.invalid"]);
    let tree = git(&source, &["hash-object", "-t", "tree", "-w", "/dev/null"]);
    let oid = git(&source, &["commit-tree", &tree, "-m", "SHA256"]);
    git(&source, &["update-ref", "refs/heads/main", &oid]);
    let text = std::fs::read_to_string(&f.config)
        .unwrap()
        .replace(
            "[repositories.project.remotes.origin]",
            "[repositories.project]\nobject_format='sha256'\n[repositories.project.remotes.origin]",
        )
        .replace(f.remote.to_str().unwrap(), source.to_str().unwrap());
    std::fs::write(&f.config, text).unwrap();
    f.ok();
    assert_eq!(oid.len(), 64);
    assert_eq!(
        git(
            &f.root.join("repos/project.git"),
            &["rev-parse", "--show-object-format"]
        ),
        "sha256"
    );
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
    cfg.limits.max_part_count = 2;
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
    assert!(parts[0].2.contains("oversized_item"));
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
    check_fetch_shutdown(false, false);
}

#[cfg(unix)]
#[test]
fn sigterm_preserves_unpublished_selected_branch_attempt() {
    check_fetch_shutdown(true, false);
}

#[cfg(unix)]
#[test]
fn sigterm_terminates_pipe_holders_after_fetch_parent_exits() {
    check_fetch_shutdown(false, true);
}

#[cfg(unix)]
fn check_fetch_shutdown(selected: bool, orphan: bool) {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(false);
    if selected {
        f.branches("branches={include=['main']}");
    }
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "After interrupted fetch",
    );
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
    let stall = if orphan {
        format!(
            "(sleep 0.2; touch '{}'; sleep 30) & exit 0",
            marker.display()
        )
    } else {
        format!("touch '{}'; sleep 30", marker.display())
    };
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *' fetch '*) {stall};; esac\nexec '{}' \"$@\"\n",
            actual.trim()
        ),
    )
    .unwrap();
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
    assert_eq!(f.count("events"), 1);
}

#[test]
fn immutable_cache_reuses_objects_and_never_caches_refs() {
    let f = Fixture::new(false);
    f.ok();
    let oid = git(&f.work, &["rev-parse", "HEAD"]);
    let cfg = Config::load(&f.config).unwrap();
    let mut repository = Git {
        path: repository_path(&cfg.storage.repositories_dir, "project"),
        execution: cfg.execution,
        limits: cfg.limits,
        shutdown: Shutdown::default(),
    };
    git_monitor::metrics::reset();
    let first = repository.metadata(&oid).unwrap();
    let operations = git_monitor::metrics::snapshot().git_processes;
    assert_eq!(first, repository.metadata(&oid).unwrap());
    assert_eq!(git_monitor::metrics::snapshot().git_processes, operations);
    repository.refs("origin").unwrap();
    repository.refs("origin").unwrap();
    assert!(git_monitor::metrics::snapshot().git_processes >= operations + 2);
    repository.execution.share_computations = false;
    repository.metadata(&oid).unwrap();
    assert!(git_monitor::metrics::snapshot().git_processes > operations + 2);
}
#[test]
fn shared_analysis_has_the_same_content_as_naive_analysis() {
    let f = Fixture::new(false);
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value >= 0"),
        "Shared computation",
    );
    let cfg = Config::load(&f.config).unwrap();
    let mut store = Store::open(&cfg.storage.state_dir).unwrap();
    git_monitor::monitor::collect(&cfg, &mut store, &Shutdown::default(), true).unwrap();
    let (_, mut task) = store.jobs().unwrap().remove(0);
    let shared = git_monitor::monitor::analyze(&task).unwrap();
    task.execution.share_computations = false;
    let naive = git_monitor::monitor::analyze(&task).unwrap();
    assert_eq!(
        serde_json::to_value(&shared.commits).unwrap(),
        serde_json::to_value(&naive.commits).unwrap()
    );
}
#[test]
fn thousand_bindings_share_a_computation_and_cached_bindings_migrate() {
    let f = Fixture::new(false);
    let mut cfg = Config::load(&f.config).unwrap();
    let original = cfg.watches[0].clone();
    for i in 1..1000 {
        let mut watch = original.clone();
        watch.id = format!("watch_{i}");
        cfg.watches.push(watch);
    }
    std::fs::write(&f.config, toml::to_string(&cfg).unwrap()).unwrap();
    f.ok();
    f.change(
        &BEFORE.replace("value > 0", "value > 1000"),
        "Shared thousand",
    );
    let output = f.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("analyses=1"));
    assert_eq!(f.count("computations"), 1);
    assert_eq!(f.count("watch_jobs"), 1000);
    assert_eq!(f.count("matches"), 1000);
    f.db().execute("DELETE FROM watch_jobs", []).unwrap();
    f.db().execute("DELETE FROM computations", []).unwrap();
    f.db()
        .execute("UPDATE jobs SET state='pending'", [])
        .unwrap();
    let output = f.run();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("analyses=0"));
    assert_eq!(f.count("watch_jobs"), 1000);
}
