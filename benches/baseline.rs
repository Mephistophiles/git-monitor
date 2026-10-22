use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use git_monitor::{
    config::{DiffConfig, Limits},
    diff,
};

fn baseline(c: &mut Criterion) {
    let old = "int check(int value) {\n    return value > 0;\n}\n".repeat(30);
    let new = old.replace("value > 0", "value >= 0");
    let config = DiffConfig {
        mode: "text".into(),
        ..Default::default()
    };
    c.bench_function("native_text_diff_90_lines", |b| {
        b.iter(|| {
            diff::build(
                "check.c",
                black_box(old.as_bytes()),
                black_box(new.as_bytes()),
                1,
                1,
                None,
                &config,
                &Limits::default(),
            )
        })
    });
    let syntax = DiffConfig::default();
    c.bench_function("native_syntax_diff_90_lines", |b| {
        b.iter(|| {
            diff::build(
                "check.c",
                black_box(old.as_bytes()),
                black_box(new.as_bytes()),
                1,
                1,
                None,
                &syntax,
                &Limits::default(),
            )
        })
    });
}

fn incremental_fixture(branches: &str, consumers: usize) -> tempfile::TempDir {
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(args)
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "Benchmark"]);
    git(&["config", "user.email", "bench@example.invalid"]);
    std::fs::write(root.path().join("source.c"), "int value = 1;\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "Initial"]);
    let cfg = format!("version=1\n[storage]\nrepositories_dir='cache'\nstate_dir='state'\n[defaults]\ndigest_every='0s'\n[recipients.bench]\nto=['bench@example.invalid']\n[repositories.bench.remotes.origin]\nurl='{}'\n[[watches]]\nid='bench'\nrepository='bench'\nremotes=['origin']\nrecipients=['bench']\nbranches={branches}\npaths=['*.c']\n", root.path().display());
    let template = cfg.split("[[watches]]").nth(1).unwrap().to_owned();
    let mut cfg = cfg;
    for i in 1..consumers {
        cfg.push_str(&format!(
            "\n[[watches]]{}",
            template.replace("id='bench'", &format!("id='bench_{i}'"))
        ));
    }
    std::fs::write(root.path().join("monitor.toml"), cfg).unwrap();
    std::fs::write(root.path().join("mail.toml"), "version=1\n[smtp]\nhost='localhost'\nport=2525\nsecurity='none'\nfrom='bench@example.invalid'\ntimeout='1s'\n").unwrap();
    run_pass(root.path());
    std::fs::write(root.path().join("source.c"), "int value = 2;\n").unwrap();
    git(&["add", "source.c"]);
    git(&["commit", "-qm", "Changed"]);
    root
}

fn run_pass(root: &std::path::Path) {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("git-monitor");
    let output = std::process::Command::new(binary)
        .args(["--config"])
        .arg(root.join("monitor.toml"))
        .arg("--mail-config")
        .arg(root.join("mail.toml"))
        .args(["run", "--once", "--force", "--prepare-only"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end");
    group
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(2));
    group.bench_function("incremental_fetch_to_mime", |b| {
        b.iter_batched(
            || incremental_fixture("{all=true}", 1),
            |root| {
                run_pass(root.path());
                black_box(root)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("incremental_selected_fetch_to_mime", |b| {
        b.iter_batched(
            || incremental_fixture("{include=['main']}", 1),
            |root| {
                run_pass(root.path());
                black_box(root)
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}
fn filter_scaling(c: &mut Criterion) {
    use std::collections::BTreeSet;
    let cfg: git_monitor::config::Config =
        toml::from_str(include_str!("../examples/monitor.toml")).unwrap();
    let mut group = c.benchmark_group("canonical_filter_grouping");
    group
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(100))
        .measurement_time(std::time::Duration::from_millis(200));
    for n in [1, 10, 100, 1000] {
        for distinct in [false, true] {
            let watches: Vec<_> = (0..n)
                .map(|i| {
                    let mut w = cfg.watches[0].clone();
                    w.id = format!("consumer_{i}");
                    if distinct {
                        w.paths = Some(vec![format!("file_{i}.c")]);
                        w.function = None;
                    }
                    w
                })
                .collect();
            group.bench_function(
                format!("{}_n_{n}", if distinct { "distinct" } else { "repeated" }),
                |b| {
                    b.iter(|| {
                        let keys: BTreeSet<_> = watches.iter().map(|w| w.selector_key()).collect();
                        assert_eq!(keys.len(), if distinct { n } else { 1 });
                        black_box(keys)
                    })
                },
            );
        }
    }
    group.finish();
}
fn repeated_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end_repeated_filters");
    group
        .sample_size(10)
        .warm_up_time(std::time::Duration::from_millis(100))
        .measurement_time(std::time::Duration::from_secs(1));
    group.bench_function("10_watch_bindings_one_analysis", |b| {
        b.iter_batched(
            || incremental_fixture("{all=true}", 10),
            |root| {
                run_pass(root.path());
                let db =
                    rusqlite::Connection::open(root.path().join("state/state.sqlite3")).unwrap();
                assert_eq!(
                    db.query_row("SELECT count(*) FROM computations", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    1
                );
                assert_eq!(
                    db.query_row("SELECT count(*) FROM watch_jobs", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    10
                );
                black_box(root)
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}
criterion_group!(
    benches,
    baseline,
    end_to_end,
    filter_scaling,
    repeated_end_to_end
);
criterion_main!(benches);
