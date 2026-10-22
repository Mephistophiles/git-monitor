use crate::{
    git::Git,
    metrics::Counters,
    store::{Attempt, Snapshot, Store, Task},
};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, sync::mpsc};
type Job = Box<dyn FnOnce(&mut Store) + Send>;
pub struct Actor {
    tx: mpsc::SyncSender<Job>,
}
pub fn scope<R>(store: &mut Store, work: impl FnOnce(&Actor) -> R) -> R {
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<Job>(32);
        scope.spawn(move || {
            while let Ok(job) = rx.recv() {
                job(store)
            }
        });
        let actor = Actor { tx };
        work(&actor)
    })
}
impl Actor {
    pub fn call<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Store) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(Box::new(move |store| {
                crate::metrics::reset();
                let result = work(store);
                let _ = tx.send((result, crate::metrics::snapshot()));
            }))
            .map_err(|_| anyhow::anyhow!("SQLite worker stopped"))?;
        let (result, counters) = rx.recv().context("SQLite command interrupted")?;
        crate::metrics::merge(&counters);
        result
    }
    pub fn activate_source(
        &self,
        repository: &str,
        remote: &str,
        identity: &str,
    ) -> Result<String> {
        let (repository, remote, identity) = (
            repository.to_owned(),
            remote.to_owned(),
            identity.to_owned(),
        );
        self.call(move |s| s.activate_source(&repository, &remote, &identity))
    }
    pub fn snapshot(&self, key: &str) -> Result<Option<Snapshot>> {
        let key = key.to_owned();
        self.call(move |s| s.snapshot(&key))
    }
    pub fn next_poll(&self, key: &str) -> Result<u64> {
        let key = key.to_owned();
        self.call(move |s| s.next_poll(&key))
    }
    pub fn begin(&self, a: &Attempt) -> Result<i64> {
        let a = a.clone();
        self.call(move |s| s.begin(&a))
    }
    pub fn update_attempt(&self, id: i64, a: &Attempt) -> Result<()> {
        let a = a.clone();
        self.call(move |s| s.update_attempt(id, &a))
    }
    pub fn finish(
        &self,
        id: i64,
        a: &Attempt,
        refs: BTreeMap<String, String>,
        git: &Git,
    ) -> Result<usize> {
        let (a, git) = (a.clone(), git.clone());
        self.call(move |s| s.finish(id, &a, refs, &git))
    }
    pub fn record_metrics(&self, stage: &str, counters: &Counters) -> Result<()> {
        let (stage, counters) = (stage.to_owned(), counters.clone());
        self.call(move |s| s.record_metrics(&stage, &counters))
    }
    pub fn fetch_error(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        self.call(move |s| s.fetch_error(&key))
    }
    pub fn claim_computation(
        &self,
        key: &str,
        bindings: &[(i64, Task)],
        seconds: u64,
    ) -> Result<String> {
        let (key, bindings) = (key.to_owned(), bindings.to_owned());
        self.call(move |s| s.claim_computation(&key, &bindings, seconds))
    }
}
