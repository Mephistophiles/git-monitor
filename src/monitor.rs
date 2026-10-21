use crate::{
    config::{duration, Config, Watch},
    diff,
    git::{repository_path, Changed, Git},
    mail::{self, MailConfig},
    model::{CommitView, EventView, FileDiff, Report},
    process::{self, Shutdown},
    render,
    store::{source_key, Analysis, Attempt, Store, Task},
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
            let mut fd = diff::build(
                &f.path,
                a.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                b.as_ref().map_or(&[], |s| s.bytes.as_slice()),
                a.as_ref().map_or(1, |s| s.start),
                b.as_ref().map_or(1, |s| s.start),
                None,
                &task.diff,
                &task.limits,
            );
            fd.kind = match (a.is_some(), b.is_some()) {
                (false, true) => "function_appeared",
                (true, false) => "function_disappeared",
                _ => "function_modified",
            }
            .into();
            fd.note = format!(
                "{} · scoped function diff · selector {}",
                fd.note, f.expression
            );
            files.push(fd);
        }
    } else {
        for change in changes.iter().filter(|f| w.path_matches(&f.path)) {
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
                    git.show_patch(new, &change.path)?
                } else {
                    git.raw(&[
                        "diff",
                        "--no-color",
                        "--no-renames",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--diff-algorithm=histogram",
                        "--unified=3",
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
    let git = Git {
        path: task.path.clone(),
        execution: task.execution.clone(),
        limits: task.limits.clone(),
        shutdown: Shutdown::default(),
    };
    let mut result = Analysis::default();
    if task.event.kind == "branch_created" || task.event.kind == "branch_deleted" {
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
        let changes = git.changed(Some(old), new)?;
        let (files, notes) = selected(&git, task, Some(old), new, &changes, false)?;
        result.files = files;
        result.notes.extend(notes);
        result.notes.push(
            "History rewritten: aggregate old → new comparison; commit history is a DAG.".into(),
        );
    }
    Ok(result)
}

pub fn recover(store: &mut Store, shutdown: &Shutdown) -> Result<usize> {
    let mut count = 0;
    for (id, attempt) in store.attempts()? {
        let git = attempt.git(shutdown.clone());
        let refs = git.refs(&attempt.remote)?;
        if refs == attempt.published {
            store.db.execute("DELETE FROM attempts WHERE id=?", [id])?;
        } else {
            count += store.finish(id, &attempt, refs, &git)?;
        }
    }
    Ok(count)
}

pub fn collect(
    cfg: &Config,
    store: &mut Store,
    shutdown: &Shutdown,
    force: bool,
) -> Result<(usize, usize)> {
    let mut events = recover(store, shutdown)?;
    let mut failures = 0;
    for (repository, r) in &cfg.repositories {
        if shutdown.requested() {
            break;
        }
        let git = Git {
            path: repository_path(&cfg.storage.repositories_dir, repository),
            execution: cfg.execution.clone(),
            limits: cfg.limits.clone(),
            shutdown: shutdown.clone(),
        };
        git.init()?;
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
            let key = source_key(repository, remote, &source.url);
            let snapshot = store.snapshot(&key)?;
            let interval = duration(
                source
                    .poll_every
                    .as_deref()
                    .unwrap_or(&cfg.defaults.poll_every),
            )?
            .as_secs();
            if !force && crate::now() < store.next_poll(&key)? {
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
            let attempt = Attempt {
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
            };
            for oid in attempt.old.values() {
                git.pin(oid)?;
            }
            git.configure_remote(remote, &source.url)?;
            let id = store.begin(&attempt)?;
            match git.fetch(remote, !attempt.initialized) {
                Ok(()) => {
                    let refs = git.refs(remote)?;
                    events += store.finish(id, &attempt, refs, &git)?;
                    eprintln!("checked {repository}/{remote}");
                }
                Err(_) => {
                    if shutdown.requested() {
                        // Keep the attempt: deadline expiry must not start fresh Git work.
                        break;
                    }
                    failures += 1;
                    recover(store, shutdown)?;
                    store.fetch_error(&key)?;
                    eprintln!("fetch failed: {repository}/{remote}; local result reconciled");
                }
            }
        }
    }
    Ok((events, failures))
}

pub fn process_jobs(cfg: &Config, store: &mut Store, shutdown: &Shutdown) -> Result<usize> {
    let tmp = cfg.storage.state_dir.join("tmp");
    std::fs::create_dir_all(&tmp)?;
    let mut analyses = 0;
    for (id, mut task) in store.jobs()? {
        task.execution = cfg.execution.clone();
        task.limits = cfg.limits.clone();
        task.diff = cfg.diff.clone();
        if shutdown.requested() {
            break;
        }
        let key = crate::hash(serde_json::to_vec(&(
            &task.event.repository,
            &task.path,
            &task.event.old,
            &task.event.new,
            &task.event.kind,
            task.watch.selector_key(),
            &task.diff,
            &task.limits,
        ))?);
        if let Some(result) = store.cache(&key)? {
            store.finish_job(id, &task, &key, &result, None, cfg)?;
            continue;
        }
        store
            .db
            .execute("UPDATE jobs SET state='running' WHERE id=?", [id])?;
        let input = tmp.join(format!("job-{id}.json"));
        let output = tmp.join(format!("result-{id}.json"));
        crate::atomic_write(&input, &serde_json::to_vec(&task)?)?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("worker")
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output);
        let run = process::run(
            &mut cmd,
            duration(&cfg.limits.analysis_timeout)?,
            65536,
            shutdown,
            duration(&cfg.execution.shutdown_grace)?,
            Some(cfg.limits.max_worker_address_space_bytes),
        );
        analyses += 1;
        let result = run.and_then(|o| {
            ensure!(
                o.code == 0,
                "analysis worker failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            let size = std::fs::metadata(&output)?.len();
            ensure!(
                size <= cfg.limits.max_job_output_bytes as u64,
                "analysis output budget exceeded"
            );
            Ok(serde_json::from_slice::<Analysis>(&std::fs::read(
                &output,
            )?)?)
        });
        match result {
            Ok(result) => store.finish_job(id, &task, &key, &result, None, cfg)?,
            Err(e) => {
                if shutdown.requested() {
                    store
                        .db
                        .execute("UPDATE jobs SET state='pending' WHERE id=?", [id])?;
                } else {
                    let error = e.to_string();
                    let result = Analysis {
                        notes: vec![format!("analysis_failed: {error}")],
                        ..Default::default()
                    };
                    store.finish_job(id, &task, &key, &result, Some(&error), cfg)?;
                    eprintln!("analysis job {id} failed");
                }
            }
        }
        let _ = std::fs::remove_file(input);
        let _ = std::fs::remove_file(output);
    }
    Ok(analyses)
}

fn allowed(cfg: &Config, watch: &Watch, event: &EventView, key: &str) -> Option<Watch> {
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
    (source_key(&event.repository, &event.remote, &source.url) == key).then(|| current.clone())
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
    for m in store.matches(recipient, pending_only, cfg.limits.max_job_output_bytes)? {
        let Some(w) = allowed(cfg, &m.watch, &m.event, &m.key) else {
            store.db.execute(
                "UPDATE matches SET digest='revoked' WHERE id=? AND digest IS NULL",
                [m.id],
            )?;
            continue;
        };
        if !w.recipients.iter().any(|r| r == recipient) {
            continue;
        }
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
    mail_config: Option<&MailConfig>,
    shutdown: &Shutdown,
    output: Option<&Path>,
) -> Result<usize> {
    let artifacts = cfg.storage.state_dir.join("artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let mut queued = 0;
    for (recipient, r) in &cfg.recipients {
        if let Some(mail) = mail_config {
            let revision = mail::revision(cfg, recipient, mail);
            let stale: Vec<String> = store.db.prepare("SELECT DISTINCT digest FROM deliveries WHERE recipient=? AND revision<>? AND state NOT IN ('sent','revoked')")?.query_map(params![recipient, revision], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            for digest in stale {
                let tx = store.db.transaction()?;
                tx.execute(
                    "UPDATE deliveries SET state='revoked' WHERE digest=? AND state<>'sent'",
                    [&digest],
                )?;
                tx.execute("UPDATE matches SET digest=NULL WHERE digest=?", [&digest])?;
                tx.execute(
                    "UPDATE clocks SET last_digest=0 WHERE recipient=?",
                    [recipient],
                )?;
                tx.commit()?;
            }
        }
        let (pending, ids) = report(cfg, store, recipient, mail_config.is_some())?;
        let html = if pending.events.is_empty() && mail_config.is_some() {
            render::html(&report(cfg, store, recipient, false)?.0, &cfg.diff)
        } else {
            render::html(&pending, &cfg.diff)
        };
        crate::atomic_write(
            &artifacts.join(format!("{recipient}-latest.html")),
            html.as_bytes(),
        )?;
        if let Some(dir) = output {
            crate::atomic_write(&dir.join(format!("{recipient}.html")), html.as_bytes())?;
        }
        let Some(mail) = mail_config else {
            continue;
        };
        let revision = mail::revision(cfg, recipient, mail);
        let interval = duration(
            r.digest_every
                .as_deref()
                .unwrap_or(&cfg.defaults.digest_every),
        )?
        .as_secs();
        let last = store.clock(recipient)?;
        if pending.events.is_empty()
            || crate::now().saturating_sub(last) < interval
            || shutdown.requested()
        {
            continue;
        }
        let parts = mail::package(&pending, r, mail, cfg)?;
        let tx = store.db.transaction()?;
        let digest = crate::hash(serde_json::to_vec(&(
            &revision,
            &ids,
            pending.generated_at,
        ))?);
        for (index, (id, mime, html)) in parts.into_iter().enumerate() {
            let artifact = artifacts.join(format!("{digest}-{}.html", index + 1));
            crate::atomic_write(&artifact, html.as_bytes())?;
            tx.execute("INSERT INTO deliveries(id,recipient,revision,digest,mime) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,digest=excluded.digest,mime=excluded.mime,state='pending' WHERE deliveries.state='revoked'",params![id,recipient,revision,digest,mime])?;
            queued += 1;
        }
        for id in ids {
            tx.execute(
                "UPDATE matches SET digest=? WHERE id=?",
                params![digest, id],
            )?;
        }
        tx.execute(
            "UPDATE clocks SET last_digest=? WHERE recipient=?",
            params![crate::now(), recipient],
        )?;
        tx.commit()?;
    }
    Ok(queued)
}

pub fn send(
    cfg: &Config,
    store: &Store,
    mail: &MailConfig,
    shutdown: &Shutdown,
    force: bool,
) -> Result<usize> {
    let rows: Vec<(String,String,String)> = store.db.prepare("SELECT id,recipient,revision FROM deliveries WHERE state IN ('pending','uncertain') AND (? OR next_attempt<=?) ORDER BY rowid LIMIT 100")?.query_map(params![force,crate::now()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<std::result::Result<_,_>>()?;
    let mut sent = 0;
    for (id, recipient, revision) in rows {
        if shutdown.requested() {
            break;
        }
        let Some(r) = cfg.recipients.get(&recipient) else {
            store
                .db
                .execute("UPDATE deliveries SET state='revoked' WHERE id=?", [&id])?;
            continue;
        };
        if mail::revision(cfg, &recipient, mail) != revision {
            continue;
        }
        let mime: Vec<u8> =
            store
                .db
                .query_row("SELECT mime FROM deliveries WHERE id=?", [&id], |r| {
                    r.get(0)
                })?;
        let message = cfg.storage.state_dir.join("tmp/smtp-message.eml");
        crate::atomic_write(&message, &mime)?;
        store.db.execute(
            "UPDATE deliveries SET state='sending',attempts=attempts+1 WHERE id=?",
            [&id],
        )?;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("smtp-worker")
            .arg("--message")
            .arg(&message)
            .env("GIT_MONITOR_SMTP", serde_json::to_string(&mail.smtp)?)
            .env("GIT_MONITOR_TO", serde_json::to_string(&r.to)?);
        let result = process::run(
            &mut cmd,
            duration(&mail.smtp.timeout)? + Duration::from_secs(2),
            65536,
            shutdown,
            duration(&cfg.execution.shutdown_grace)?,
            None,
        );
        if result.is_ok_and(|o| o.code == 0) {
            store.db.execute(
                "UPDATE deliveries SET state='sent',error=NULL WHERE id=?",
                [&id],
            )?;
            sent += 1;
        } else {
            store.db.execute("UPDATE deliveries SET state='uncertain',error='SMTP attempt incomplete; duplicate possible',next_attempt=? + min(3600,5 * (1 << min(attempts-1,9))) WHERE id=?",params![crate::now(),id])?;
        }
        let _ = std::fs::remove_file(&message);
    }
    Ok(sent)
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
