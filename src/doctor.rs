//! Offline capability probes: no configured remote or SMTP endpoint is contacted.
use crate::{
    config::Config,
    git::Git,
    process::{self, Shutdown},
};
use anyhow::{ensure, Result};

pub fn check(cfg: &Config) -> Result<String> {
    let shutdown = Shutdown::default();
    let dir = tempfile::tempdir()?;
    let source = Git {
        path: dir.path().join("source.git"),
        execution: cfg.execution.clone(),
        limits: cfg.limits.clone(),
        shutdown: shutdown.clone(),
    };
    source.init()?;
    source.raw(&["config", "user.name", "Git Monitor capability probe"])?;
    source.raw(&["config", "user.email", "probe@localhost"])?;
    let blob = String::from_utf8(source.raw_input(
        &["hash-object", "-w", "--stdin"],
        b"int probe(void) {\n    return 1;\n}\n".to_vec(),
    )?)?
    .trim()
    .to_owned();
    let tree = String::from_utf8(source.raw_input(
        &["mktree"],
        format!("100644 blob {blob}\tprobe.c\n").into_bytes(),
    )?)?
    .trim()
    .to_owned();
    let commit = source.text(&["commit-tree", &tree, "-m", "capability probe"])?;
    let commit = commit.trim();
    source.raw(&["update-ref", "refs/heads/probe", commit])?;
    source.raw(&["check-attr", "--source", commit, "--all", "--", "probe.c"])?;
    source.show_patch(commit, "probe.c", 0)?;
    ensure!(
        source
            .select(
                Some(commit),
                &crate::config::Function {
                    path: "probe.c".into(),
                    expression: "probe".into()
                }
            )?
            .is_some(),
        "Git -L probe failed"
    );
    let cache = Git {
        path: dir.path().join("cache.git"),
        ..source.clone()
    };
    cache.init()?;
    cache.configure_remote("probe", source.path.to_str().unwrap())?;
    cache.fetch("probe", true)?;
    ensure!(
        cache.refs("probe")?.get("probe").map(String::as_str) == Some(commit),
        "atomic fetch probe failed"
    );
    let marker = cache.write_manifest(b"capability probe".to_vec())?;
    cache.publish_selected(
        "probe",
        &cache.refs("probe")?,
        &Default::default(),
        "refs/git-monitor/probe",
        &marker,
        None,
    )?;
    source.raw(&["update-ref", "-d", "refs/heads/probe"])?;
    cache.fetch("probe", false)?;
    ensure!(
        cache.refs("probe")?.is_empty(),
        "atomic pruning of final branch unsupported"
    );
    let output = process::run(
        std::process::Command::new("git").arg("--version"),
        crate::config::duration(&cfg.execution.git_timeout)?,
        65536,
        &shutdown,
        crate::config::duration(&cfg.execution.shutdown_grace)?,
        Some(cfg.limits.max_worker_address_space_bytes),
    )?;
    ensure!(
        output.code == 0,
        "mandatory worker memory limit unavailable"
    );
    for directory in [&cfg.storage.state_dir, &cfg.storage.repositories_dir] {
        std::fs::create_dir_all(directory)?;
        let mut probe = tempfile::NamedTempFile::new_in(directory)?;
        use std::io::Write;
        probe.write_all(b"git-monitor")?;
        probe.as_file().sync_all()?;
    }
    let database = cfg.storage.state_dir.join("state.sqlite3");
    if database.exists() {
        let db = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let integrity: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        ensure!(
            integrity == "ok",
            "state integrity check failed: {integrity}"
        );
        let orphaned: i64 = db.query_row(
            "SELECT count(*) FROM jobs j LEFT JOIN events e ON e.id=j.event_id WHERE e.id IS NULL",
            [],
            |r| r.get(0),
        )?;
        ensure!(orphaned == 0, "orphaned analysis jobs in state database");
    }
    Ok(format!("{}\nCapabilities: atomic fetch/prune, update-ref transactions, first-parent show, tree attributes, Git -L; worker limits, storage and queue integrity OK\nModel {} · dependencies {}", crate::monitor::doctor()?, crate::profile::MODEL_VERSION,
        &crate::profile::AnalysisProfile::freeze(&cfg.execution.diff_drivers)?.dependencies[..12]))
}
