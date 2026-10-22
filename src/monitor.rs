use crate::{
    config::{duration, Config, Watch},
    diff,
    git::{repository_path, Changed, Git},
    mail::{self, MailConfig},
    model::{CommitView, EventView, FileDiff, Report},
    process::{self, Shutdown},
    store::{Analysis, Attempt, FetchPlan, NarrowObservation, Store, Task},
};
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use rusqlite::params;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::Path,
    process::Command,
    sync::Arc,
    time::Duration,
};

pub struct Locks {
    _files: Vec<File>,
}
impl Locks {
    pub fn acquire(cfg: &Config) -> Result<Self> {
        std::fs::create_dir_all(&cfg.storage.state_dir)?;
        std::fs::create_dir_all(&cfg.storage.repositories_dir)?;
        let mut files = vec![];
        let state_lock = cfg.storage.state_dir.join("instance.lock");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&state_lock)?;
        f.try_lock_exclusive()
            .context("another instance owns state_dir")?;
        files.push(f);
        let mut repositories: BTreeSet<_> = cfg
            .repositories
            .keys()
            .map(|id| repository_path(&cfg.storage.repositories_dir, id))
            .collect();
        let database = cfg.storage.state_dir.join("state.sqlite3");
        if database.exists() {
            let db = rusqlite::Connection::open_with_flags(
                database,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='attempts')",
                [],
                |r| r.get(0),
            )?;
            if exists {
                let mut q = db.prepare("SELECT context FROM attempts")?;
                for row in q.query_map([], |r| r.get::<_, String>(0))? {
                    let attempt: Attempt = serde_json::from_str(&row?)?;
                    repositories.insert(attempt.path);
                }
            }
        }
        let mut directories: BTreeSet<_> = repositories
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        directories.insert(cfg.storage.repositories_dir.clone());
        let state = std::fs::canonicalize(&cfg.storage.state_dir)?;
        for directory in directories {
            let owner = directory.join(".git-monitor-owner");
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&owner)?;
            f.try_lock_exclusive()
                .context("managed repository directory is already in use")?;
            let mut stored = Vec::new();
            f.read_to_end(&mut stored)?;
            if stored.is_empty() {
                f.write_all(state.as_os_str().as_encoded_bytes())?;
                f.sync_all()?;
            } else {
                ensure!(
                    stored.as_slice() == state.as_os_str().as_encoded_bytes(),
                    "repository directory belongs to a different state_dir"
                );
            }
            files.push(f);
        }
        for repository in repositories {
            let path = repository.with_extension("lock");
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            f.try_lock_exclusive()
                .with_context(|| format!("another instance owns {}", path.display()))?;
            files.push(f);
        }
        Ok(Self { _files: files })
    }
}

fn relevant_attributes(changes: &[Changed], path: &str) -> bool {
    changes.iter().any(|f| {
        f.path == ".gitattributes"
            || f.path
                .strip_suffix("/.gitattributes")
                .is_some_and(|dir| path.starts_with(&format!("{dir}/")))
    })
}
fn attrs(git: &Git, rev: Option<&str>, path: &str) -> Result<String> {
    match rev {
        None => Ok(String::new()),
        Some(r) => Ok(crate::hash(git.raw(&[
            "check-attr",
            "--source",
            r,
            "-z",
            "--all",
            "--",
            path,
        ])?)),
    }
}

fn selected(
    git: &Git,
    task: &Task,
    old: Option<&str>,
    new: &str,
    changes: &[Changed],
    commit_patch: bool,
) -> Result<(Vec<FileDiff>, Vec<String>)> {
    let w = &task.watch;
    let profile = if task.execution.analysis_profile.git.is_empty() {
        crate::profile::AnalysisProfile::freeze(&task.execution.diff_drivers)?
    } else {
        task.execution.analysis_profile.clone()
    };
    let mut files = vec![];
    let mut notes = vec![];
    if let Some(f) = &w.function {
        let rules = relevant_attributes(changes, &f.path)
            && attrs(git, old, &f.path)? != attrs(git, Some(new), &f.path)?;
        if !rules && !changes.iter().any(|c| c.path == f.path) {
            return Ok((files, notes));
        }
        let a = git.select(old, f)?;
        let b = git.select(Some(new), f)?;
        if rules {
            notes.push(format!(
                "analysis_rules_changed: {} ({} → {})",
                f.path,
                attrs(git, old, &f.path)?,
                attrs(git, Some(new), &f.path)?
            ));
        }
        if a.as_ref().map(|s| &s.bytes) != b.as_ref().map(|s| &s.bytes) {
            let patch = git.snippet_patch(
                a.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                b.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                task.diff.context_lines,
            )?;
            let mut fd = diff::build(
                &f.path,
                a.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                b.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                a.as_ref().map_or(1, |s| s.start),
                b.as_ref().map_or(1, |s| s.start),
                Some(&patch),
                &task.diff,
                &task.limits,
            );
            fd.canonical_patch = None;
            fd.kind = match (a.is_some(), b.is_some()) {
                (false, true) => "function_appeared",
                (true, false) => "function_disappeared",
                _ => "function_modified",
            }
            .into();
            if let Some(document) = &mut fd.document {
                document.analysis_profile = Some(profile.clone());
                document.old.blob = a.as_ref().map(|s| s.oid.clone());
                document.new.blob = b.as_ref().map(|s| s.oid.clone());
                let old_offset = a.as_ref().map_or(0, |s| s.offset);
                let new_offset = b.as_ref().map_or(0, |s| s.offset);
                document.old.original_range.start += old_offset;
                document.old.original_range.end += old_offset;
                document.new.original_range.start += new_offset;
                document.new.original_range.end += new_offset;
                document
                    .quality
                    .push("scoped_function_diff_not_a_whole_file_patch".into());
            }
            fd.note = format!(
                "{} · scoped function diff · selector {}",
                fd.note, f.expression
            );
            files.push(fd);
        }
    } else {
        let oids = changes
            .iter()
            .filter(|c| w.path_matches(&c.path) && c.old_mode != "160000" && c.new_mode != "160000")
            .flat_map(|c| c.old_blob.iter().chain(&c.new_blob))
            .cloned()
            .collect();
        git.prefetch_blobs(&oids)?;
        for change in changes.iter().filter(|f| {
            w.path_matches(&f.path)
                && task
                    .previous_watch
                    .as_ref()
                    .is_none_or(|previous| previous.path_matches(&f.path))
        }) {
            if change.old_mode == "160000" || change.new_mode == "160000" {
                let mut f = FileDiff::diagnostic(
                    &change.path,
                    format!(
                        "gitlink {} → {}",
                        change.old_blob.as_deref().unwrap_or("∅"),
                        change.new_blob.as_deref().unwrap_or("∅")
                    ),
                );
                f.kind = "gitlink".into();
                files.push(f);
                continue;
            }
            let make = || -> Result<FileDiff> {
                let a = git.blob(change.old_blob.as_deref())?;
                let b = git.blob(change.new_blob.as_deref())?;
                if a.contains(&0) || b.contains(&0) {
                    let mut f = FileDiff::diagnostic(
                        &change.path,
                        "Binary object changed; content is not embedded",
                    );
                    f.kind = "binary".into();
                    return Ok(f);
                }
                let patch = if commit_patch {
                    git.show_patch(new, &change.path, task.diff.context_lines)?
                } else {
                    git.raw(&[
                        "diff",
                        "--no-color",
                        "--no-renames",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--diff-algorithm=histogram",
                        &format!("--unified={}", task.diff.context_lines),
                        old.context("aggregate diff needs an old revision")?,
                        new,
                        "--",
                        &change.path,
                    ])?
                };
                let mut f = diff::build(
                    &change.path,
                    &a,
                    &b,
                    1,
                    1,
                    Some(&patch),
                    &task.diff,
                    &task.limits,
                );
                if a == b {
                    f.kind = "mode_changed".into();
                    f.note = format!("File mode {} → {}", change.old_mode, change.new_mode);
                }
                if let Some(document) = &mut f.document {
                    document.analysis_profile = Some(profile.clone());
                    document.old.blob = change.old_blob.clone();
                    document.new.blob = change.new_blob.clone();
                    document.old.mode = change.old_mode.clone();
                    document.new.mode = change.new_mode.clone();
                }
                Ok(f)
            };
            match make() {
                Ok(f) => files.push(f),
                Err(e) if e.to_string().contains("budget exceeded") => {
                    files.push(FileDiff::diagnostic(&change.path, e.to_string()))
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok((files, notes))
}

pub fn analyze(task: &Task) -> Result<Analysis> {
    crate::structural::set_cache(
        task.execution
            .share_computations
            .then(|| task.path.join("git-monitor-syntax-cache")),
    );
    let git = Git {
        path: task.path.clone(),
        execution: task.execution.clone(),
        limits: task.limits.clone(),
        shutdown: Shutdown::default(),
    };
    let mut result = Analysis::default();
    if task.event.kind == "branch_created" {
        if let Some(base) = &task.event.comparison_base {
            let new = task.event.new.as_deref().context("missing new OID")?;
            let changes = git.changed(Some(&base.oid), new)?;
            let (files, notes) = selected(&git, task, Some(&base.oid), new, &changes, false)?;
            result.files = files;
            result.notes = notes;
            result.notes.push(format!(
                "Comparison against explicit base {}/{} @ {}; branch origin is not inferred",
                base.remote, base.branch, base.oid
            ));
        }
        return Ok(result);
    }
    if task.event.kind == "branch_deleted" {
        return Ok(result);
    }
    let old = task.event.old.as_deref().context("missing old OID")?;
    let new = task.event.new.as_deref().context("missing new OID")?;
    for oid in git.commits(old, new)? {
        let parent = git.parent(&oid)?;
        // Enumerate every commit; first-parent controls its patch, not graph traversal.
        let changes = git.changed(None, &oid)?;
        let (files, notes) = selected(&git, task, parent.as_deref(), &oid, &changes, true)?;
        result.notes.extend(notes);
        if !files.is_empty() {
            let (subject, author) = git.metadata(&oid)?;
            result.commits.push(CommitView {
                oid,
                subject,
                author,
                files,
            });
        }
    }
    if task.event.kind == "history_rewritten" {
        result.outgoing = git.commits(new, old)?;
    }
    {
        let changes = git.changed(Some(old), new)?;
        let (files, notes) = selected(&git, task, Some(old), new, &changes, false)?;
        result.files = files;
        result.notes.extend(notes);
        if task.event.kind == "history_rewritten" {
            result.notes.push(
                "History rewritten: aggregate old → new comparison; commit history is a DAG."
                    .into(),
            );
        }
    }
    Ok(result)
}

pub fn recover(store: &mut Store, shutdown: &Shutdown) -> Result<usize> {
    let mut count = 0;
    for (id, attempt) in store.attempts()? {
        let git = attempt.git(shutdown.clone());
        let refs = git.refs(&attempt.remote)?;
        let published = match &attempt.plan {
            FetchPlan::All => refs != attempt.published,
            FetchPlan::Selected { observation } => {
                let complete = if let Some(observation) = observation {
                    let complete =
                        git.ref_oid(&attempt.marker_ref())?.as_deref() == Some(&observation.marker);
                    if complete {
                        ensure!(
                            refs == observation.refs,
                            "published refs disagree with observation manifest"
                        );
                    }
                    complete
                } else {
                    false
                };
                git.cleanup_staging(&attempt.staging_prefix(id))?;
                complete
            }
        };
        if !published {
            store.db.execute("DELETE FROM attempts WHERE id=?", [id])?;
        } else {
            count += store.finish(id, &attempt, refs, &git)?;
        }
    }
    Ok(count)
}

// Keep the confirmed manifest durable before any download or publication. This
// method deliberately stops before SQLite finalization, which recovery can repeat.
pub fn fetch_attempt(
    store: &Store,
    id: i64,
    attempt: &mut Attempt,
    git: &Git,
) -> Result<BTreeMap<String, String>> {
    fetch_attempt_journal(id, attempt, git, |a| store.update_attempt(id, a))
}
fn fetch_attempt_journal(
    id: i64,
    attempt: &mut Attempt,
    git: &Git,
    journal: impl Fn(&Attempt) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    if matches!(attempt.plan, FetchPlan::All) {
        git.fetch(&attempt.remote, !attempt.initialized)?;
        return git.refs(&attempt.remote);
    }
    let staging = attempt.staging_prefix(id);
    for _ in 0..3 {
        git.cleanup_staging(&staging)?;
        let mut desired = git.discover(&attempt.remote, !attempt.initialized)?;
        desired.retain(|branch, _| attempt.watches.iter().any(|w| w.branch_matches(branch)));
        let previous_marker = git.ref_oid(&attempt.marker_ref())?;
        let marker = git.write_manifest(serde_json::to_vec(&(
            &attempt.key,
            id,
            &desired,
            &previous_marker,
        ))?)?;
        attempt.plan = FetchPlan::Selected {
            observation: Some(NarrowObservation {
                refs: desired.clone(),
                marker: marker.clone(),
                previous_marker: previous_marker.clone(),
            }),
        };
        journal(attempt)?;
        match git.download_selected(&attempt.remote, &staging, &desired, !attempt.initialized) {
            Ok(true) => {
                git.publish_selected(
                    &attempt.remote,
                    &attempt.published,
                    &desired,
                    &attempt.marker_ref(),
                    &marker,
                    previous_marker.as_deref(),
                )?;
                git.cleanup_staging(&staging)?;
                return Ok(desired);
            }
            Ok(false) | Err(_) if !git.shutdown.requested() => continue,
            Ok(false) => anyhow::bail!("discovered objects became unavailable"),
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("remote changed during discovery/download or download failed; retry on next poll")
}

pub fn collect(
    cfg: &Config,
    store: &mut Store,
    shutdown: &Shutdown,
    force: bool,
) -> Result<(usize, usize)> {
    crate::persistence::scope(store, |shared| collect_actor(cfg, shared, shutdown, force))
}
fn collect_actor(
    cfg: &Config,
    shared: &crate::persistence::Actor,
    shutdown: &Shutdown,
    force: bool,
) -> Result<(usize, usize)> {
    let stop = shutdown.clone();
    let recovered = shared.call(move |s| recover(s, &stop))?;
    let frozen = cfg.clone();
    shared.call(move |s| s.retire_disabled_sources(&frozen))?;
    let hosts = crate::scheduler::Hosts::new(cfg.execution.max_fetches_per_host);
    let results = crate::scheduler::map(
        cfg.repositories.iter().collect(),
        cfg.execution.max_fetches,
        |(repository, r)| -> Result<(usize, usize)> {
            let (mut events, mut failures) = (0, 0);
            if shutdown.requested() {
                return Ok((0, 0));
            }
            let git = Git {
                path: repository_path(&cfg.storage.repositories_dir, repository),
                execution: cfg.execution.clone(),
                limits: cfg.limits.clone(),
                shutdown: shutdown.clone(),
            };
            git.init_format(&r.object_format)?;
            let comparison_base = if cfg.defaults.new_branch_mode == "compare" {
                let remote = cfg
                    .defaults
                    .base_remote
                    .as_deref()
                    .context("missing base_remote")?;
                let branch = cfg
                    .defaults
                    .base_branch
                    .as_deref()
                    .context("missing base_branch")?;
                let source = &r.remotes[remote];
                let Some(_permit) = hosts.acquire(&source.url, shutdown) else {
                    return Ok((0, 0));
                };
                git.configure_remote(remote, &source.url)?;
                Some(crate::model::ComparisonBase {
                    remote: remote.into(),
                    branch: branch.into(),
                    oid: git.fetch_base(remote, branch)?,
                })
            } else {
                None
            };
            for (remote, source) in &r.remotes {
                if shutdown.requested() {
                    break;
                }
                let watches: Vec<_> = cfg
                    .watches
                    .iter()
                    .filter(|w| w.repository == *repository && w.remotes.contains(remote))
                    .cloned()
                    .collect();
                if watches.is_empty() {
                    continue;
                }
                let key = shared.activate_source(
                    repository,
                    remote,
                    &crate::store::source_identity(repository, remote, source),
                )?;
                let snapshot = shared.snapshot(&key)?;
                let interval = duration(
                    source
                        .poll_every
                        .as_deref()
                        .unwrap_or(&cfg.defaults.poll_every),
                )?
                .as_secs();
                let interval = crate::scheduler::poll_interval(&key, interval);
                if !force && crate::now() < shared.next_poll(&key)? {
                    continue;
                }
                if !force
                    && snapshot
                        .as_ref()
                        .is_some_and(|(_, _, t)| crate::now().saturating_sub(*t) < interval)
                {
                    continue;
                }
                // Preserve every currently published object before replacing a source URL.
                let published = git.refs(remote)?;
                for oid in published.values() {
                    git.pin(oid)?;
                }
                let plan = FetchPlan::for_watches(&watches);
                let mut attempt = Attempt {
                    key: key.clone(),
                    repository: repository.clone(),
                    remote: remote.clone(),
                    path: git.path.clone(),
                    old: snapshot.as_ref().map(|s| s.0.clone()).unwrap_or_default(),
                    published,
                    initialized: snapshot.is_some(),
                    previous: snapshot.as_ref().map(|s| s.1.clone()).unwrap_or_default(),
                    watches,
                    execution: cfg.execution.clone(),
                    limits: cfg.limits.clone(),
                    diff: cfg.diff.clone(),
                    plan,
                    comparison_base: comparison_base.clone(),
                };
                for oid in attempt.old.values() {
                    git.pin(oid)?;
                }
                git.configure_remote(remote, &source.url)?;
                let Some(_permit) = hosts.acquire(&source.url, shutdown) else {
                    break;
                };
                let id = shared.begin(&attempt)?;
                match fetch_attempt_journal(id, &mut attempt, &git, |a| {
                    shared.update_attempt(id, a)
                }) {
                    Ok(refs) => {
                        events += shared.finish(id, &attempt, refs, &git)?;
                        eprintln!("checked {repository}/{remote}");
                    }
                    Err(_) => {
                        if shutdown.requested() {
                            // Keep the attempt: deadline expiry must not start fresh Git work.
                            break;
                        }
                        failures += 1;
                        shared.fetch_error(&key)?;
                        eprintln!("fetch failed: {repository}/{remote}; local result reconciled");
                    }
                }
            }
            Ok((events, failures))
        },
    );
    let (mut events, mut failures, mut error) = (recovered, 0, None);
    for result in results {
        match result {
            Ok((e, f)) => {
                events += e;
                failures += f;
            }
            Err(e) => error = Some(e),
        }
    }
    if !shutdown.requested() {
        let stop = shutdown.clone();
        events += shared.call(move |s| recover(s, &stop))?;
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok((events, failures))
}

pub fn analysis_key(cfg: &Config, task: &Task) -> Result<String> {
    Ok(crate::hash(serde_json::to_vec(&(
        if cfg.execution.analysis_profile.git.is_empty() {
            crate::profile::AnalysisProfile::freeze(&cfg.execution.diff_drivers)?.key()
        } else {
            cfg.execution.analysis_profile.key()
        },
        &task.event.repository,
        &task.path,
        &task.event.old,
        &task.event.new,
        &task.event.kind,
        &task.event.comparison_base,
        task.watch.selector_key(),
        task.previous_watch.as_ref().map(Watch::selector_key),
        &task.diff,
        &task.limits,
    ))?))
}
pub fn process_jobs(cfg: &Config, store: &mut Store, shutdown: &Shutdown) -> Result<usize> {
    crate::persistence::scope(store, |shared| process_jobs_actor(cfg, shared, shutdown))
}
fn process_jobs_actor(
    cfg: &Config,
    shared: &crate::persistence::Actor,
    shutdown: &Shutdown,
) -> Result<usize> {
    let tmp = cfg.storage.state_dir.join("tmp");
    std::fs::create_dir_all(&tmp)?;
    let frozen = Arc::new(cfg.clone());
    let mut groups: BTreeMap<String, Vec<(i64, Task)>> = BTreeMap::new();
    for (id, mut task) in shared.call(|s| s.jobs())? {
        task.execution = cfg.execution.clone();
        task.limits = cfg.limits.clone();
        task.diff = cfg.diff.clone();
        let mut key = analysis_key(cfg, &task)?;
        if !cfg.execution.share_computations {
            key = crate::hash(format!("{key}:{id}"));
        }
        groups.entry(key).or_default().push((id, task));
    }
    let results = crate::scheduler::map(
        groups.into_iter().collect(),
        cfg.execution.max_analyses,
        |(key, bindings)| -> Result<usize> {
            if shutdown.requested() {
                return Ok(0);
            }
            let task = &bindings[0].1;
            if cfg.execution.share_computations {
                let (key, bindings, cfg) = (key.clone(), bindings.clone(), frozen.clone());
                if shared.call(move |s| {
                    if let Some(result) = s.cache(&key)? {
                        for (id, task) in &bindings {
                            s.finish_job(*id, task, &key, &result, None, &cfg)?;
                        }
                        return Ok(true);
                    }
                    Ok(false)
                })? {
                    crate::metrics::update(|s| s.analysis_cache_hits += 1);
                    return Ok(0);
                }
            }
            let timeout = duration(&cfg.limits.analysis_timeout)?;
            let token =
                shared.claim_computation(&key, &bindings, timeout.as_secs().saturating_add(60))?;
            let input = tmp.join(format!("job-{token}.json"));
            let output = tmp.join(format!("result-{token}.json"));
            crate::atomic_write(&input, &serde_json::to_vec(task)?)?;
            let mut command = Command::new(process::worker_executable()?);
            command
                .arg("worker")
                .arg("--input")
                .arg(&input)
                .arg("--output")
                .arg(&output);
            let result = process::run(
                &mut command,
                timeout,
                65536,
                shutdown,
                duration(&cfg.execution.shutdown_grace)?,
                Some(cfg.limits.max_worker_address_space_bytes),
            )
            .and_then(|out| {
                ensure!(
                    out.code == 0,
                    "analysis worker failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                ensure!(
                    std::fs::metadata(&output)?.len() <= cfg.limits.max_job_output_bytes as u64,
                    "analysis output budget exceeded"
                );
                let result: Analysis = serde_json::from_slice(&std::fs::read(&output)?)?;
                for file in result
                    .files
                    .iter()
                    .chain(result.commits.iter().flat_map(|c| &c.files))
                {
                    file.validate()?;
                }
                Ok(result)
            });
            let cfg = frozen.clone();
            let interrupted = shutdown.requested();
            shared.call(move |s| {
                if s.owns_computation(&key, &token)? {
                    match result {
                        Ok(result) => {
                            s.record_metrics("analysis-worker", &result.counters)?;
                            for (id, task) in &bindings {
                                s.finish_job(*id, task, &key, &result, None, &cfg)?;
                            }
                            s.release_computation(&key, &token, Some(&result))?;
                        }
                        Err(error) => {
                            if interrupted {
                                for (id, _) in &bindings {
                                    s.db.execute(
                                        "UPDATE jobs SET state='pending' WHERE id=?",
                                        [id],
                                    )?;
                                }
                            } else {
                                let error = error.to_string();
                                let result = Analysis {
                                    notes: vec![format!("analysis_failed: {error}")],
                                    ..Default::default()
                                };
                                for (id, task) in &bindings {
                                    s.finish_job(*id, task, &key, &result, Some(&error), &cfg)?;
                                }
                            }
                            s.release_computation(&key, &token, None)?;
                        }
                    }
                }
                Ok(())
            })?;
            let _ = std::fs::remove_file(input);
            let _ = std::fs::remove_file(output);
            Ok(1)
        },
    );
    let mut count = 0;
    for result in results {
        count += result?;
    }
    crate::metrics::update(|s| s.analyses += count as u64);
    Ok(count)
}

fn allowed(
    cfg: &Config,
    store: &Store,
    watch: &Watch,
    event: &EventView,
    key: &str,
) -> Option<Watch> {
    let current = cfg.watches.iter().find(|w| {
        w.id == watch.id
            && w.repository == event.repository
            && w.function == watch.function
            && w.remotes.contains(&event.remote)
            && w.branch_matches(&event.branch)
            && w.events.iter().any(|kind| {
                kind == if event.kind == "branch_updated" {
                    "commits"
                } else {
                    &event.kind
                }
            })
    })?;
    let source = cfg
        .repositories
        .get(&event.repository)?
        .remotes
        .get(&event.remote)?;
    (store
        .active_source(
            &event.repository,
            &event.remote,
            &crate::store::source_identity(&event.repository, &event.remote, source),
        )
        .ok()?
        .as_str()
        == key)
        .then(|| current.clone())
}

pub fn report(
    cfg: &Config,
    store: &Store,
    recipient: &str,
    pending_only: bool,
) -> Result<(Report, Vec<i64>)> {
    ensure!(cfg.recipients.contains_key(recipient), "unknown recipient");
    let mut ids = vec![];
    let mut events: BTreeMap<i64, EventView> = BTreeMap::new();
    let mut scopes = BTreeMap::new();
    let matches = store.matches(
        recipient,
        pending_only,
        cfg.limits.max_job_output_bytes,
        |m| {
            if let Some(w) = allowed(cfg, store, &m.watch, &m.event, &m.key)
                .filter(|w| w.recipients.iter().any(|r| r == recipient))
            {
                scopes.insert(m.id, w);
                Ok(true)
            } else {
                store.db.execute(
                    "UPDATE matches SET digest='revoked' WHERE id=? AND digest IS NULL",
                    [m.id],
                )?;
                Ok(false)
            }
        },
    )?;
    for m in matches {
        let w = &scopes[&m.id];
        let mut e = m.event;
        for c in &mut e.commits {
            c.files.retain(|f| w.path_matches(&f.path));
        }
        e.commits.retain(|c| !c.files.is_empty());
        e.files.retain(|f| w.path_matches(&f.path));
        if e.kind == "branch_updated"
            && e.commits.is_empty()
            && e.files.is_empty()
            && e.notes.is_empty()
        {
            continue;
        }
        ids.push(m.id);
        if let Some(existing) = events.get_mut(&e.id) {
            for watch in e.watches {
                if !existing.watches.contains(&watch) {
                    existing.watches.push(watch);
                }
            }
            for c in e.commits {
                if let Some(known) = existing.commits.iter_mut().find(|x| x.oid == c.oid) {
                    for f in c.files {
                        if !known.files.iter().any(|x| {
                            crate::hash(serde_json::to_vec(x).unwrap())
                                == crate::hash(serde_json::to_vec(&f).unwrap())
                        }) {
                            known.files.push(f);
                        }
                    }
                } else {
                    existing.commits.push(c);
                }
            }
            for f in e.files {
                if !existing.files.iter().any(|x| {
                    serde_json::to_string(x).unwrap() == serde_json::to_string(&f).unwrap()
                }) {
                    existing.files.push(f);
                }
            }
            for n in e.notes {
                if !existing.notes.contains(&n) {
                    existing.notes.push(n);
                }
            }
        } else {
            events.insert(e.id, e);
        }
    }
    Ok((
        Report {
            recipient: recipient.into(),
            generated_at: crate::now(),
            events: events.into_values().collect(),
        },
        ids,
    ))
}

pub fn outputs(
    cfg: &Config,
    store: &mut Store,
    mail: Option<&MailConfig>,
    shutdown: &Shutdown,
    output: Option<&Path>,
) -> Result<usize> {
    crate::persistence::scope(store, |actor| {
        outputs_actor(cfg, actor, mail, shutdown, output)
    })
}
fn outputs_actor(
    cfg: &Config,
    actor: &crate::persistence::Actor,
    mail_config: Option<&MailConfig>,
    shutdown: &Shutdown,
    output: Option<&Path>,
) -> Result<usize> {
    let artifacts = cfg.storage.state_dir.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let mut queued = 0;
    for (recipient, r) in &cfg.recipients {
        if shutdown.requested() {
            break;
        }
        let revision = mail_config.map(|m| mail::revision(cfg, recipient, m));
        let (configuration, name, wanted) = (cfg.clone(), recipient.clone(), revision.clone());
        let(pending,ids,preview,clock)=actor.call(move|store|{let has_mail=wanted.is_some();if let Some(revision)=wanted{
   let stale:Vec<String>=store.db.prepare("SELECT DISTINCT digest FROM deliveries WHERE recipient=? AND revision<>? AND state NOT IN ('sent','revoked')")?.query_map(params![name,revision],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?;
   for digest in stale{let tx=store.db.transaction()?;tx.execute("UPDATE deliveries SET state='revoked' WHERE digest=? AND state<>'sent'",[&digest])?;tx.execute("UPDATE matches SET digest=NULL WHERE digest=?",[&digest])?;tx.execute("UPDATE clocks SET last_digest=0 WHERE recipient=?",[&name])?;tx.commit()?;}
  }let(pending,ids)=report(&configuration,store,&name,has_mail)?;let preview=if pending.events.is_empty()&&has_mail{report(&configuration,store,&name,false)?.0}else{pending.clone()};Ok((pending,ids,preview,store.clock(&name)?))})?;
        let interval = duration(
            r.digest_every
                .as_deref()
                .unwrap_or(&cfg.defaults.digest_every),
        )?
        .as_secs();
        let ready = revision.is_some()
            && !pending.events.is_empty()
            && crate::now().saturating_sub(clock) >= interval
            && !shutdown.requested();
        let prepared = match crate::prepare::bounded(
            cfg,
            &preview,
            if ready { Some(r) } else { None },
            mail_config.map(|m| m.smtp.from.as_str()),
            shutdown,
        ) {
            Ok(result) => result,
            Err(_) if shutdown.requested() => break,
            Err(error) => return Err(error),
        };
        actor.record_metrics("report-worker", &prepared.counters)?;
        crate::atomic_write(
            &artifacts.join(format!("{recipient}-latest.html")),
            prepared.html.as_bytes(),
        )?;
        if let Some(output) = output {
            crate::atomic_write(
                &output.join(format!("{recipient}.html")),
                prepared.html.as_bytes(),
            )?;
        }
        if !ready {
            continue;
        }
        let revision = revision.unwrap();
        let digest = crate::hash(serde_json::to_vec(&(
            &revision,
            &ids,
            pending.generated_at,
        ))?);
        let mut messages = vec![];
        for (index, (id, mime, html)) in prepared.parts.into_iter().enumerate() {
            crate::atomic_write(
                &artifacts.join(format!("{digest}-{}.html", index + 1)),
                html.as_bytes(),
            )?;
            messages.push((id, mime));
        }
        queued += messages.len();
        let recipient = recipient.clone();
        actor.call(move|store|{let tx=store.db.transaction()?;tx.execute("INSERT OR IGNORE INTO delivery_times VALUES(?,?)",params![digest,crate::now()])?;for(id,mime)in messages{tx.execute("INSERT INTO deliveries(id,recipient,revision,digest,mime) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,digest=excluded.digest,mime=excluded.mime,state='pending' WHERE deliveries.state='revoked'",params![id,recipient,revision,digest,mime])?;}for id in ids{tx.execute("UPDATE matches SET digest=? WHERE id=?",params![digest,id])?;}tx.execute("UPDATE clocks SET last_digest=? WHERE recipient=?",params![crate::now(),recipient])?;tx.commit()?;Ok(())})?;
    }
    Ok(queued)
}
pub fn send(
    cfg: &Config,
    store: &mut Store,
    mail: &MailConfig,
    shutdown: &Shutdown,
    force: bool,
) -> Result<usize> {
    crate::persistence::scope(store, |actor| send_actor(cfg, actor, mail, shutdown, force))
}
fn send_actor(
    cfg: &Config,
    actor: &crate::persistence::Actor,
    mail: &MailConfig,
    shutdown: &Shutdown,
    force: bool,
) -> Result<usize> {
    let rows:Vec<(String,String,String)>=actor.call(move|store|Ok(store.db.prepare("SELECT id,recipient,revision FROM deliveries WHERE state IN ('pending','uncertain') AND (? OR next_attempt<=?) ORDER BY rowid LIMIT 100")?.query_map(params![force,crate::now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?))?;
    let mut sent = 0;
    for (id, recipient, revision) in rows {
        if shutdown.requested() {
            break;
        }
        let Some(r) = cfg.recipients.get(&recipient) else {
            actor.call(move |store| {
                store
                    .db
                    .execute("UPDATE deliveries SET state='revoked' WHERE id=?", [id])?;
                Ok(())
            })?;
            continue;
        };
        if mail::revision(cfg, &recipient, mail) != revision {
            continue;
        }
        let key = id.clone();
        let mime: Vec<u8> = actor.call(move |store| {
            Ok(store
                .db
                .query_row("SELECT mime FROM deliveries WHERE id=?", [key], |r| {
                    r.get(0)
                })?)
        })?;
        let message = cfg.storage.state_dir.join("tmp/smtp-message.eml");
        crate::atomic_write(&message, &mime)?;
        let key = id.clone();
        actor.call(move |store| {
            store.db.execute(
                "UPDATE deliveries SET state='sending',attempts=attempts+1 WHERE id=?",
                [key],
            )?;
            Ok(())
        })?;
        let mut command = Command::new(process::worker_executable()?);
        command
            .arg("smtp-worker")
            .arg("--message")
            .arg(&message)
            .env("GIT_MONITOR_SMTP", serde_json::to_string(&mail.smtp)?)
            .env("GIT_MONITOR_TO", serde_json::to_string(&r.to)?);
        let success = process::run(
            &mut command,
            duration(&mail.smtp.timeout)? + Duration::from_secs(2),
            65536,
            shutdown,
            duration(&cfg.execution.shutdown_grace)?,
            None,
        )
        .is_ok_and(|o| o.code == 0);
        actor.call(move|store|{if success{store.db.execute("UPDATE deliveries SET state='sent',error=NULL WHERE id=?",[id])?;}else{store.db.execute("UPDATE deliveries SET state='uncertain',error='SMTP attempt incomplete; duplicate possible',next_attempt=? + min(3600,5 * (1 << min(attempts-1,9))) WHERE id=?",params![crate::now(),id])?;}Ok(())})?;
        if success {
            sent += 1;
        }
        let _ = std::fs::remove_file(message);
    }
    Ok(sent)
}
#[allow(clippy::too_many_arguments)]
pub fn daemon(
    cfg: &Config,
    store: &mut Store,
    mail: Option<&MailConfig>,
    shutdown: &Shutdown,
    force: bool,
    prepare_only: bool,
    output: Option<&Path>,
) -> Result<()> {
    struct Guard<'a>(&'a Shutdown);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            if !self.0.requested() {
                self.0.request();
            }
        }
    }
    recover(store, shutdown)?;
    crate::persistence::scope(store, |actor| {
        std::thread::scope(|threads| {
            let mut phases = vec![];
            for role in ["fetch", "analysis", "report", "smtp"] {
                phases.push(threads.spawn(move || -> Result<()> {
                    let _guard = Guard(shutdown);
                    let mut first = true;
                    while !shutdown.requested() {
                        crate::metrics::reset();
                        let result = match role {
                            "fetch" => {
                                collect_actor(cfg, actor, shutdown, force && first).map(|_| ())
                            }
                            "analysis" => process_jobs_actor(cfg, actor, shutdown).map(|_| ()),
                            "report" => {
                                outputs_actor(cfg, actor, mail, shutdown, output).map(|_| ())
                            }
                            _ => {
                                if let Some(mail) = mail.filter(|_| !prepare_only) {
                                    send_actor(cfg, actor, mail, shutdown, false).map(|_| ())
                                } else {
                                    Ok(())
                                }
                            }
                        };
                        if let Err(error) = result {
                            shutdown.request();
                            return Err(error.context(format!("{role} phase")));
                        }
                        actor.record_metrics(role, &crate::metrics::snapshot())?;
                        crate::metrics::log(role);
                        first = false;
                        process::pause(
                            if role == "analysis" || role == "smtp" {
                                Duration::from_millis(100)
                            } else {
                                Duration::from_secs(1)
                            },
                            shutdown,
                        )?;
                    }
                    Ok(())
                }));
            }
            let mut failure = None;
            for phase in phases {
                match phase.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => failure = Some(e),
                    Err(_) => {
                        shutdown.request();
                        failure = Some(anyhow::anyhow!("phase panicked"));
                    }
                }
            }
            if let Some(error) = failure {
                Err(error)
            } else {
                Ok(())
            }
        })
    })
}

pub fn doctor() -> Result<String> {
    let o = Command::new("git").arg("--version").output()?;
    ensure!(o.status.success(), "Git unavailable");
    let version = String::from_utf8(o.stdout)?;
    let numbers: Vec<u32> = version
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    ensure!(
        numbers.first() == Some(&2) && numbers.get(1).is_some_and(|x| *x >= 51),
        "v0.1 requires Git 2.51 or newer"
    );
    Ok(format!(
        "{}SQLite bundled; native TextEngine/SyntaxEngine; C/C++/Rust/Python/JSON grammars",
        version
    ))
}
