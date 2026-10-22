//! Retention under the same instance/repository locks as collection.
//! Incomplete work and unsent mail never expire through age alone.
use crate::{
    config::Config,
    git::{repository_path, Git},
    process::Shutdown,
    store::{Store, Task},
};
use anyhow::{ensure, Result};
use rusqlite::params;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Debug, Default, serde::Serialize)]
pub struct Summary {
    pub events: usize,
    pub files: usize,
    pub pins: usize,
}

fn clean(directory: &Path, age: u64, keep: &BTreeSet<String>) -> Result<usize> {
    if !directory.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        // Do not follow links or recurse into a directory owned by somebody else.
        if !entry.file_type()?.is_file()
            || keep.contains(&entry.file_name().to_string_lossy().into_owned())
        {
            continue;
        }
        let old = metadata
            .modified()?
            .elapsed()
            .map(|d| d.as_secs() >= age)
            .unwrap_or(false);
        if old {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
pub fn run(cfg: &Config, store: &mut Store, shutdown: &Shutdown, gc: bool) -> Result<Summary> {
    crate::monitor::recover(store, shutdown)?;
    ensure!(
        store.attempts()?.is_empty(),
        "maintenance requires all fetch attempts to be reconciled"
    );
    let cutoff = crate::now().saturating_sub(cfg.maintenance.history_days.saturating_mul(86400));
    let eligible:Vec<i64>=store.db.prepare("SELECT e.id FROM events e WHERE json_extract(e.body,'$.observed_at')<? AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.event_id=e.id AND j.state<>'done') AND NOT EXISTS(SELECT 1 FROM matches m WHERE m.event_id=e.id AND (m.digest IS NULL OR EXISTS(SELECT 1 FROM deliveries d WHERE d.digest=m.digest AND d.state NOT IN ('sent','revoked'))))")?
        .query_map([cutoff],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?;
    let tx = store.db.transaction()?;
    for id in &eligible {
        tx.execute("DELETE FROM matches WHERE event_id=?", [id])?;
        tx.execute("DELETE FROM jobs WHERE event_id=?", [id])?;
        tx.execute("DELETE FROM events WHERE id=?", [id])?;
    }
    tx.execute("DELETE FROM deliveries WHERE state IN ('sent','revoked') AND digest IN (SELECT digest FROM delivery_times WHERE created<?) AND NOT EXISTS(SELECT 1 FROM matches m WHERE m.digest=deliveries.digest)",[cutoff])?;
    tx.execute("DELETE FROM delivery_times WHERE NOT EXISTS(SELECT 1 FROM deliveries d WHERE d.digest=delivery_times.digest)",[])?;
    // Cache is disposable, unlike recorded matches and delivery attempts.
    tx.execute(
        "DELETE FROM cache WHERE key NOT IN (SELECT key FROM cache ORDER BY rowid DESC LIMIT ?)",
        [cfg.maintenance.cache_items as u64],
    )?;
    tx.execute("DELETE FROM cache WHERE rowid IN (SELECT rowid FROM (SELECT rowid,SUM(length(body)) OVER(ORDER BY rowid DESC) AS bytes FROM cache) WHERE bytes>?)",[cfg.maintenance.cache_bytes])?;
    tx.execute("DELETE FROM computations WHERE NOT EXISTS(SELECT 1 FROM watch_jobs w WHERE w.computation=computations.key)",[])?;
    tx.commit()?;
    let mut roots: BTreeMap<std::path::PathBuf, BTreeSet<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&cfg.storage.repositories_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().extension().is_some_and(|s| s == "git") {
            roots.entry(entry.path()).or_default();
        }
    }
    let jobs: Vec<String> = store
        .db
        .prepare("SELECT task FROM jobs")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    for body in jobs {
        let task: Task = serde_json::from_str(&body)?;
        roots
            .entry(task.path)
            .or_default()
            .extend(task.event.old.into_iter().chain(task.event.new));
    }
    // Snapshots may be needed when an old source is re-enabled. They are tiny;
    // keep their tips while the corresponding snapshot is retained.
    for id in cfg.repositories.keys() {
        let path = repository_path(&cfg.storage.repositories_dir, id);
        let target = roots.entry(path).or_default();
        for body in store.db.prepare("SELECT refs FROM snapshots WHERE key IN (SELECT key FROM active_sources WHERE repository=?)")?
            .query_map([id],|r|r.get::<_,String>(0))? {
            let refs:BTreeMap<String,String>=serde_json::from_str(&body?)?;target.extend(refs.into_values());
        }
    }
    let mut pins = 0;
    for (path, keep) in roots {
        if shutdown.requested() {
            break;
        }
        if !path.exists() {
            continue;
        }
        let git = Git {
            path,
            execution: cfg.execution.clone(),
            limits: cfg.limits.clone(),
            shutdown: shutdown.clone(),
        };
        let refs = git.text(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/git-monitor/pins/",
        ])?;
        let mut input = String::from("start\n");
        for line in refs.lines() {
            let (name, oid) = line.split_once(' ').unwrap();
            if !keep.contains(oid) {
                input.push_str(&format!("delete {name} {oid}\n"));
                pins += 1;
            }
        }
        input.push_str("prepare\ncommit\n");
        git.raw_input(&["update-ref", "--stdin"], input.into_bytes())?;
        crate::cache::prune(
            &git.path.join("git-monitor-cache"),
            cfg.maintenance.cache_items,
            cfg.maintenance.cache_bytes,
        )?;
        if gc {
            git.raw(&["commit-graph", "write", "--reachable"])?;
            git.raw(&["repack", "-ad"])?;
            git.raw(&["gc", "--prune=now"])?;
        }
    }
    let digests: BTreeSet<String> = store
        .db
        .prepare("SELECT DISTINCT digest FROM deliveries WHERE state NOT IN ('sent','revoked')")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let artifacts = cfg.storage.state_dir.join("artifacts");
    let mut keep = BTreeSet::new();
    if artifacts.exists() {
        for entry in std::fs::read_dir(&artifacts)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with("-latest.html") || digests.iter().any(|d| name.starts_with(d)) {
                keep.insert(name);
            }
        }
    }
    crate::cache::prune(
        &cfg.storage.state_dir.join("render-cache"),
        cfg.maintenance.cache_items,
        cfg.maintenance.cache_bytes,
    )?;
    let files = clean(
        &artifacts,
        cfg.maintenance.artifact_days.saturating_mul(86400),
        &keep,
    )? + clean(
        &cfg.storage.state_dir.join("tmp"),
        cfg.maintenance.temporary_hours.saturating_mul(3600),
        &BTreeSet::new(),
    )?;
    store.db.execute(
        "INSERT OR REPLACE INTO maintenance_log(id,body) VALUES(1,?)",
        params![serde_json::to_string(&(
            crate::now(),
            eligible.len(),
            files,
            pins
        ))?],
    )?;
    Ok(Summary {
        events: eligible.len(),
        files,
        pins,
    })
}
