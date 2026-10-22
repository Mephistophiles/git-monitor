use crate::{
    config::{Config, DiffConfig, Execution, Limits, Watch},
    git::Git,
    model::{CommitView, EventView, FileDiff},
    process::Shutdown,
};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub key: String,
    pub repository: String,
    pub remote: String,
    pub path: PathBuf,
    pub old: BTreeMap<String, String>,
    pub published: BTreeMap<String, String>,
    pub initialized: bool,
    pub watches: Vec<Watch>,
    pub previous: BTreeMap<String, Watch>,
    pub execution: Execution,
    pub limits: Limits,
    pub diff: DiffConfig,
    #[serde(default)]
    pub plan: FetchPlan,
    #[serde(default)]
    pub comparison_base: Option<crate::model::ComparisonBase>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FetchPlan {
    #[default]
    All,
    Selected {
        observation: Option<NarrowObservation>,
    },
}
impl FetchPlan {
    pub fn for_watches(watches: &[Watch]) -> Self {
        if watches
            .iter()
            .any(|w| w.branches.all && w.branches.exclude.is_empty())
        {
            Self::All
        } else {
            Self::Selected { observation: None }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrowObservation {
    pub refs: BTreeMap<String, String>,
    pub marker: String,
    pub previous_marker: Option<String>,
}
impl Attempt {
    pub fn marker_ref(&self) -> String {
        format!("refs/git-monitor/observations/{}", self.key)
    }
    pub fn staging_prefix(&self, id: i64) -> String {
        format!("refs/git-monitor/fetch/{}/{id}/", self.key)
    }
}
impl Attempt {
    pub fn git(&self, shutdown: Shutdown) -> Git {
        Git {
            path: self.path.clone(),
            execution: self.execution.clone(),
            limits: self.limits.clone(),
            shutdown,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub source_key: String,
    pub path: PathBuf,
    pub event: EventView,
    pub watch: Watch,
    /// Restrict the first transition after a selector change to continuous scope.
    /// Older persisted jobs already carry their complete selector in `watch`.
    #[serde(default)]
    pub previous_watch: Option<Watch>,
    pub execution: Execution,
    pub limits: Limits,
    pub diff: DiffConfig,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Analysis {
    #[serde(default)]
    pub counters: crate::metrics::Counters,
    pub commits: Vec<CommitView>,
    pub files: Vec<FileDiff>,
    pub outgoing: Vec<String>,
    pub notes: Vec<String>,
}
#[derive(Debug)]
pub struct PendingMatch {
    pub id: i64,
    pub key: String,
    pub watch: Watch,
    pub event: EventView,
}

pub struct Store {
    pub db: Connection,
}
pub type Snapshot = (BTreeMap<String, String>, BTreeMap<String, Watch>, u64);
pub fn source_key(repository: &str, remote: &str, url: &str) -> String {
    crate::hash(format!("{repository}\0{remote}\0{url}"))
}
pub fn source_identity(repository: &str, remote: &str, source: &crate::config::Remote) -> String {
    if source.access_context.is_empty() {
        source_key(repository, remote, &source.url)
    } else {
        crate::hash(
            serde_json::to_vec(&(repository, remote, &source.url, &source.access_context)).unwrap(),
        )
    }
}

impl Store {
    pub fn open(state: &Path) -> Result<Self> {
        std::fs::create_dir_all(state)?;
        let db = Connection::open(state.join("state.sqlite3"))?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version <= 3, "state database is newer than this binary");
        db.execute_batch("BEGIN;
          CREATE TABLE IF NOT EXISTS snapshots(key TEXT PRIMARY KEY, refs TEXT NOT NULL, watches TEXT NOT NULL, checked INTEGER NOT NULL, error TEXT);
          CREATE TABLE IF NOT EXISTS attempts(id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE, context TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS remote_status(key TEXT PRIMARY KEY, repository TEXT NOT NULL, remote TEXT NOT NULL, last_attempt INTEGER NOT NULL DEFAULT 0, last_success INTEGER, next_poll INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0, error TEXT);
          CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY, source_key TEXT NOT NULL, body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL, watch_id TEXT NOT NULL, task TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending', error TEXT, UNIQUE(event_id,watch_id));
          CREATE TABLE IF NOT EXISTS cache(key TEXT PRIMARY KEY, body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS matches(id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL, watch_id TEXT NOT NULL, recipient TEXT NOT NULL, source_key TEXT NOT NULL, watch TEXT NOT NULL, body TEXT NOT NULL, digest TEXT, UNIQUE(event_id,watch_id,recipient));
          CREATE TABLE IF NOT EXISTS deliveries(id TEXT PRIMARY KEY, recipient TEXT NOT NULL, revision TEXT NOT NULL, digest TEXT NOT NULL, mime BLOB NOT NULL, state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0, error TEXT);
          CREATE TABLE IF NOT EXISTS clocks(recipient TEXT PRIMARY KEY, last_digest INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS active_sources(repository TEXT NOT NULL, remote TEXT NOT NULL, identity TEXT NOT NULL, key TEXT NOT NULL, epoch INTEGER NOT NULL, PRIMARY KEY(repository,remote));
          CREATE TABLE IF NOT EXISTS maintenance_log(id INTEGER PRIMARY KEY,body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS delivery_times(digest TEXT PRIMARY KEY,created INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS stage_metrics(id INTEGER PRIMARY KEY,stage TEXT NOT NULL,created INTEGER NOT NULL,body TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS computations(key TEXT PRIMARY KEY,generation INTEGER NOT NULL,token TEXT NOT NULL,state TEXT NOT NULL,expires INTEGER NOT NULL,result_hash TEXT);
          CREATE TABLE IF NOT EXISTS watch_jobs(job_id INTEGER PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,computation TEXT NOT NULL,selector TEXT NOT NULL);
          PRAGMA user_version=3; COMMIT;")?;
        db.execute(
            "INSERT OR IGNORE INTO delivery_times SELECT DISTINCT digest,? FROM deliveries",
            [crate::now()],
        )?;
        db.execute("UPDATE computations SET state='pending',generation=generation+1,token='' WHERE state='running'",[])?;
        // A single instance owns the DB; any running jobs were interrupted.
        db.execute("UPDATE jobs SET state='pending' WHERE state='running'", [])?;
        db.execute(
            "UPDATE deliveries SET state='uncertain' WHERE state='sending'",
            [],
        )?;
        Ok(Self { db })
    }
    pub fn snapshot(&self, key: &str) -> Result<Option<Snapshot>> {
        let row: Option<(String, String, u64)> = self
            .db
            .query_row(
                "SELECT refs,watches,checked FROM snapshots WHERE key=?",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        row.map(|(r, w, t)| Ok((serde_json::from_str(&r)?, serde_json::from_str(&w)?, t)))
            .transpose()
    }
    pub fn activate_source(
        &self,
        repository: &str,
        remote: &str,
        identity: &str,
    ) -> Result<String> {
        let old: Option<(String, String, i64)> = self
            .db
            .query_row(
                "SELECT identity,key,epoch FROM active_sources WHERE repository=? AND remote=?",
                params![repository, remote],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((previous, key, _)) = &old {
            if previous == identity {
                return Ok(key.clone());
            }
        }
        let epoch = old.map_or(0, |(_, _, epoch)| epoch + 1);
        let key = if epoch == 0 {
            identity.to_owned()
        } else {
            crate::hash(format!("{identity}\0{epoch}"))
        };
        self.db.execute("INSERT INTO active_sources VALUES(?,?,?,?,?) ON CONFLICT(repository,remote) DO UPDATE SET identity=excluded.identity,key=excluded.key,epoch=excluded.epoch",
            params![repository,remote,identity,key,epoch])?;
        Ok(key)
    }
    pub fn retire_disabled_sources(&self, cfg: &Config) -> Result<()> {
        let rows: Vec<(String, String)> = self
            .db
            .prepare(
                "SELECT repository,remote FROM active_sources WHERE identity NOT LIKE 'inactive:%'",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (repository, remote) in rows {
            if !cfg
                .repositories
                .get(&repository)
                .is_some_and(|r| r.remotes.contains_key(&remote))
                || !cfg
                    .watches
                    .iter()
                    .any(|w| w.repository == repository && w.remotes.contains(&remote))
            {
                self.db.execute("UPDATE active_sources SET identity='inactive:' || identity WHERE repository=? AND remote=?",params![repository,remote])?;
            }
        }
        Ok(())
    }
    pub fn active_source(&self, repository: &str, remote: &str, identity: &str) -> Result<String> {
        Ok(self
            .db
            .query_row(
                "SELECT key FROM active_sources WHERE repository=? AND remote=? AND identity=?",
                params![repository, remote, identity],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| identity.to_owned()))
    }
    pub fn begin_poll(&self, key: &str, repository: &str, remote: &str) -> Result<()> {
        self.db.execute("INSERT INTO remote_status(key,repository,remote,last_attempt,next_poll) VALUES(?,?,?,?,?) ON CONFLICT(key) DO UPDATE SET last_attempt=excluded.last_attempt,next_poll=excluded.next_poll",params![key,repository,remote,crate::now(),crate::now()+1])?;
        Ok(())
    }
    pub fn begin(&self, a: &Attempt) -> Result<i64> {
        self.begin_poll(&a.key, &a.repository, &a.remote)?;
        self.db.execute(
            "INSERT INTO attempts(key,context) VALUES(?,?)",
            params![a.key, serde_json::to_string(a)?],
        )?;
        Ok(self.db.last_insert_rowid())
    }
    pub fn attempts(&self) -> Result<Vec<(i64, Attempt)>> {
        let mut q = self
            .db
            .prepare("SELECT id,context FROM attempts ORDER BY id")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, s)| Ok((id, serde_json::from_str(&s)?)))
            .collect()
    }
    pub fn update_attempt(&self, id: i64, a: &Attempt) -> Result<()> {
        ensure!(
            self.db.execute(
                "UPDATE attempts SET context=? WHERE id=? AND key=?",
                params![serde_json::to_string(a)?, id, a.key]
            )? == 1,
            "fetch attempt disappeared"
        );
        Ok(())
    }
    pub fn finish(
        &mut self,
        id: i64,
        a: &Attempt,
        new: BTreeMap<String, String>,
        git: &Git,
    ) -> Result<usize> {
        let mut drafts = vec![];
        if a.initialized {
            let names: BTreeSet<_> = a.old.keys().chain(new.keys()).cloned().collect();
            for branch in names {
                // Added scopes start at a baseline; removed scopes are not deletions.
                if !a.watches.iter().any(|w| w.branch_matches(&branch))
                    || !a.previous.values().any(|w| w.branch_matches(&branch))
                {
                    continue;
                }
                let old = a.old.get(&branch).cloned();
                let next = new.get(&branch).cloned();
                if old == next {
                    continue;
                }
                let kind = match (&old, &next) {
                    (None, Some(_)) => "branch_created",
                    (Some(_), None) => "branch_deleted",
                    (Some(o), Some(n)) => {
                        if git.ancestor(o, n)? {
                            "branch_updated"
                        } else {
                            "history_rewritten"
                        }
                    }
                    _ => unreachable!(),
                };
                drafts.push(EventView {
                    id: 0,
                    repository: a.repository.clone(),
                    remote: a.remote.clone(),
                    branch,
                    kind: kind.into(),
                    old,
                    new: next,
                    observed_at: crate::now(),
                    comparison_base: if kind == "branch_created" {
                        a.comparison_base.clone()
                    } else {
                        None
                    },
                    watches: vec![],
                    commits: vec![],
                    outgoing: vec![],
                    files: vec![],
                    notes: vec![],
                });
            }
        }
        let count = drafts.len();
        let tx = self.db.transaction()?;
        for mut event in drafts {
            tx.execute(
                "INSERT INTO events(source_key,body) VALUES(?,?)",
                params![a.key, serde_json::to_string(&event)?],
            )?;
            event.id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE events SET body=? WHERE id=?",
                params![serde_json::to_string(&event)?, event.id],
            )?;
            for w in &a.watches {
                if !w.branch_matches(&event.branch) {
                    continue;
                }
                let Some(previous) = a.previous.get(&w.id) else {
                    continue;
                };
                // Evaluate the intersection on actual paths, rather than trying to
                // infer containment from glob strings. Function changes rebaseline.
                if !previous.branch_matches(&event.branch) || w.function != previous.function {
                    continue;
                }
                let requested = if event.kind == "branch_updated" {
                    "commits"
                } else {
                    &event.kind
                };
                if !w.events.iter().any(|x| x == requested) {
                    continue;
                }
                let task = Task {
                    source_key: a.key.clone(),
                    path: a.path.clone(),
                    event: event.clone(),
                    watch: w.clone(),
                    previous_watch: (w.selector_key() != previous.selector_key())
                        .then(|| previous.clone()),
                    execution: a.execution.clone(),
                    limits: a.limits.clone(),
                    diff: a.diff.clone(),
                };
                tx.execute(
                    "INSERT INTO jobs(event_id,watch_id,task) VALUES(?,?,?)",
                    params![event.id, w.id, serde_json::to_string(&task)?],
                )?;
            }
        }
        let watches: BTreeMap<_, _> = a
            .watches
            .iter()
            .map(|w| (w.id.clone(), w.clone()))
            .collect();
        tx.execute("INSERT INTO snapshots(key,refs,watches,checked,error) VALUES(?,?,?,?,NULL) ON CONFLICT(key) DO UPDATE SET refs=excluded.refs,watches=excluded.watches,checked=excluded.checked,error=NULL", params![a.key,serde_json::to_string(&new)?,serde_json::to_string(&watches)?,crate::now()])?;
        tx.execute(
            "UPDATE remote_status SET last_success=?,next_poll=?,failures=0,error=NULL WHERE key=?",
            params![crate::now(), crate::now() + 1, a.key],
        )?;
        tx.execute("DELETE FROM attempts WHERE id=?", [id])?;
        tx.commit()?;
        Ok(count)
    }
    pub fn fetch_error(&self, key: &str) -> Result<()> {
        self.db.execute(
            "UPDATE snapshots SET error='fetch failed; inspect remote access' WHERE key=?",
            [key],
        )?;
        self.db.execute("UPDATE remote_status SET failures=failures+1,error='fetch failed; inspect remote access',next_poll=? + min(3600, 5 * (1 << min(failures,9))) WHERE key=?",params![crate::now(),key])?;
        Ok(())
    }
    pub fn next_poll(&self, key: &str) -> Result<u64> {
        Ok(self
            .db
            .query_row(
                "SELECT next_poll FROM remote_status WHERE key=?",
                [key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
    pub fn claim_computation(
        &mut self,
        key: &str,
        bindings: &[(i64, Task)],
        seconds: u64,
    ) -> Result<String> {
        let tx = self.db.transaction()?;
        let generation: i64 = tx
            .query_row(
                "SELECT generation FROM computations WHERE key=?",
                [key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
            + 1;
        let token = crate::hash(format!(
            "{key}:{generation}:{}:{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        tx.execute("INSERT INTO computations VALUES(?,?,?,'running',?,NULL) ON CONFLICT(key) DO UPDATE SET generation=excluded.generation,token=excluded.token,state='running',expires=excluded.expires,result_hash=NULL",params![key,generation,token,crate::now().saturating_add(seconds)])?;
        for (id, task) in bindings {
            tx.execute(
                "INSERT OR REPLACE INTO watch_jobs VALUES(?,?,?)",
                params![id, key, task.watch.selector_key()],
            )?;
            tx.execute("UPDATE jobs SET state='running' WHERE id=?", [id])?;
        }
        tx.commit()?;
        Ok(token)
    }
    pub fn owns_computation(&self, key: &str, token: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM computations WHERE key=? AND token=? AND state='running' AND expires>=?)",params![key,token,crate::now()],|r|r.get(0))?)
    }
    pub fn release_computation(
        &self,
        key: &str,
        token: &str,
        result: Option<&Analysis>,
    ) -> Result<()> {
        self.db.execute("UPDATE computations SET state=?,result_hash=? WHERE key=? AND token=? AND state='running'",params![if result.is_some(){"done"}else{"pending"},result.map(|r|crate::hash(serde_json::to_vec(r).unwrap())),key,token])?;
        Ok(())
    }
    pub fn jobs(&self) -> Result<Vec<(i64, Task)>> {
        self.db.execute("UPDATE jobs SET state='pending' WHERE state='running' AND id IN (SELECT w.job_id FROM watch_jobs w JOIN computations c ON c.key=w.computation WHERE c.state='running' AND c.expires<?)",[crate::now()])?;
        self.db.execute("UPDATE computations SET state='pending',generation=generation+1,token='' WHERE state='running' AND expires<?",[crate::now()])?;
        let mut q = self
            .db
            .prepare("SELECT id,task FROM jobs WHERE state='pending' ORDER BY id")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, s)| Ok((id, serde_json::from_str(&s)?)))
            .collect()
    }
    pub fn cache(&self, key: &str) -> Result<Option<Analysis>> {
        self.db
            .query_row("SELECT body FROM cache WHERE key=?", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|s| Ok(serde_json::from_str(&s)?))
            .transpose()
    }
    pub fn finish_job(
        &mut self,
        id: i64,
        task: &Task,
        key: &str,
        result: &Analysis,
        error: Option<&str>,
        cfg: &Config,
    ) -> Result<()> {
        for file in result
            .files
            .iter()
            .chain(result.commits.iter().flat_map(|c| &c.files))
        {
            file.validate()?;
        }
        let mut event = task.event.clone();
        event.watches = vec![task.watch.id.clone()];
        event.commits = result.commits.clone();
        event.files = result.files.clone();
        event.outgoing = result.outgoing.clone();
        event.notes = result.notes.clone();
        let has_changes = !event.commits.is_empty()
            || !event.files.is_empty()
            || !event.notes.is_empty()
            || event.kind != "branch_updated";
        let current = cfg.watches.iter().find(|w| {
            w.id == task.watch.id
                && w.function == task.watch.function
                && w.branch_matches(&event.branch)
                && w.remotes.contains(&event.remote)
                && w.repository == event.repository
        });
        let tx = self.db.transaction()?;
        if error.is_none() && cfg.execution.share_computations {
            tx.execute(
                "INSERT OR IGNORE INTO computations VALUES(?,0,'','done',0,?)",
                params![key, crate::hash(serde_json::to_vec(result)?)],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO watch_jobs VALUES(?,?,?)",
                params![id, key, task.watch.selector_key()],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO cache(key,body) VALUES(?,?)",
                params![key, serde_json::to_string(result)?],
            )?;
        }
        if has_changes {
            if let Some(w) = current {
                for recipient in &task.watch.recipients {
                    if w.recipients.contains(recipient) {
                        tx.execute("INSERT OR REPLACE INTO matches(event_id,watch_id,recipient,source_key,watch,body) VALUES(?,?,?,?,?,?)",params![event.id,task.watch.id,recipient,task.source_key,serde_json::to_string(&task.watch)?,serde_json::to_string(&event)?])?;
                    }
                }
            }
        }
        tx.execute(
            "UPDATE jobs SET state=?,error=? WHERE id=?",
            params![if error.is_some() { "failed" } else { "done" }, error, id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn matches(
        &self,
        recipient: &str,
        pending_only: bool,
        budget: usize,
        mut include: impl FnMut(&PendingMatch) -> Result<bool>,
    ) -> Result<Vec<PendingMatch>> {
        let order = if pending_only { "ASC" } else { "DESC" };
        let mut q = self.db.prepare(&format!("SELECT m.id,m.source_key,m.watch,m.body,e.body FROM matches m JOIN events e ON e.id=m.event_id WHERE m.recipient=? AND (?=0 OR m.digest IS NULL) AND coalesce(m.digest,'')<>'revoked' ORDER BY m.event_id {order},m.id {order}"))?;
        let mut rows = q.query(params![recipient, pending_only])?;
        let mut used = 0usize;
        let mut results: Vec<PendingMatch> = Vec::new();
        while let Some(row) = rows.next()? {
            // Reconcile subscriptions before charging the content budget. A
            // revoked row must not prevent eligible rows later in the queue.
            let mut candidate = PendingMatch {
                id: row.get(0)?,
                key: row.get(1)?,
                watch: serde_json::from_str(&row.get::<_, String>(2)?)?,
                event: serde_json::from_str(&row.get::<_, String>(4)?)?,
            };
            if !include(&candidate)? {
                continue;
            }
            let body: String = row.get(3)?;
            if !results.is_empty() && used.saturating_add(body.len()) > budget {
                results.last_mut().unwrap().event.notes.push("Report window limited by byte budget; further changes remain in SQLite for a later digest or a larger preview budget.".into());
                break;
            }
            let mut event: EventView = if body.len() > budget {
                let mut event: EventView = serde_json::from_str(&row.get::<_, String>(4)?)?;
                event.notes.push("oversized_item: report byte budget exceeded; the full result remains in SQLite. Raise max_job_output_bytes and retry the job.".into());
                event
            } else {
                serde_json::from_str(&body)?
            };
            if event.watches.is_empty() {
                event.watches.push(candidate.watch.id.clone());
            }
            used = used.saturating_add(body.len());
            candidate.event = event;
            results.push(candidate);
            if used >= budget {
                break;
            }
        }
        Ok(results)
    }
    pub fn clock(&self, recipient: &str) -> Result<u64> {
        self.db.execute(
            "INSERT OR IGNORE INTO clocks(recipient,last_digest) VALUES(?,?)",
            params![recipient, crate::now()],
        )?;
        Ok(self.db.query_row(
            "SELECT last_digest FROM clocks WHERE recipient=?",
            [recipient],
            |r| r.get(0),
        )?)
    }
    pub fn status(&self) -> Result<String> {
        let mut out = String::new();
        for table in [
            "snapshots",
            "attempts",
            "events",
            "jobs",
            "cache",
            "matches",
            "deliveries",
        ] {
            let n: i64 = self
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            out.push_str(&format!("{table:12} {n}\n"));
        }
        let failed: i64 =
            self.db
                .query_row("SELECT count(*) FROM jobs WHERE state='failed'", [], |r| {
                    r.get(0)
                })?;
        out.push_str(&format!("failed jobs  {failed}\n"));
        let mut q = self.db.prepare("SELECT repository,remote,last_attempt,last_success,next_poll,failures,error FROM remote_status ORDER BY repository,remote")?;
        let mut rows = q.query([])?;
        while let Some(r) = rows.next()? {
            out.push_str(&format!(
                "{}/{} last_attempt={} last_success={} next_poll={} failures={} {}\n",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, Option<u64>>(3)?
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "never".into()),
                r.get::<_, u64>(4)?,
                r.get::<_, u64>(5)?,
                r.get::<_, Option<String>>(6)?.unwrap_or_default()
            ));
        }
        Ok(out)
    }
    pub fn record_metrics(&self, stage: &str, counters: &crate::metrics::Counters) -> Result<()> {
        self.db.execute(
            "INSERT INTO stage_metrics(stage,created,body) VALUES(?,?,?)",
            params![stage, crate::now(), serde_json::to_string(counters)?],
        )?;
        self.db.execute("DELETE FROM stage_metrics WHERE id NOT IN (SELECT id FROM stage_metrics ORDER BY id DESC LIMIT 1000)",[])?;
        Ok(())
    }
    pub fn retry_job(&self, id: i64) -> Result<()> {
        ensure!(
            self.db.execute(
                "UPDATE jobs SET state='pending',error=NULL WHERE id=? AND state IN ('failed','done')",
                [id]
            )? == 1,
            "no completed or failed job with that ID"
        );
        Ok(())
    }
}
