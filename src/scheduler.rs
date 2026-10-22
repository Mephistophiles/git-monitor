use crate::process::Shutdown;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Condvar, Mutex},
    time::Duration,
};
pub fn map<T: Send, R: Send>(items: Vec<T>, limit: usize, work: impl Fn(T) -> R + Sync) -> Vec<R> {
    let queue = Mutex::new(VecDeque::from(items));
    let results = Mutex::new(vec![]);
    let count = limit.max(1).min(queue.lock().unwrap().len());
    std::thread::scope(|scope| {
        for _ in 0..count {
            let (queue, results, work) = (&queue, &results, &work);
            scope.spawn(move || loop {
                let Some(item) = queue.lock().unwrap().pop_front() else {
                    break;
                };
                crate::metrics::reset();
                let result = work(item);
                results
                    .lock()
                    .unwrap()
                    .push((result, crate::metrics::snapshot()));
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|(result, counters)| {
            crate::metrics::merge(&counters);
            result
        })
        .collect()
}
pub struct Hosts {
    active: Mutex<BTreeMap<String, usize>>,
    changed: Condvar,
    limit: usize,
}
pub struct Permit<'a> {
    hosts: &'a Hosts,
    key: String,
}
impl Hosts {
    pub fn new(limit: usize) -> Self {
        Self {
            active: Mutex::new(BTreeMap::new()),
            changed: Condvar::new(),
            limit: limit.max(1),
        }
    }
    pub fn acquire(&self, url: &str, shutdown: &Shutdown) -> Option<Permit<'_>> {
        let key = host(url);
        let mut active = self.active.lock().unwrap();
        loop {
            if shutdown.requested() {
                return None;
            }
            let count = active.entry(key.clone()).or_default();
            if *count < self.limit {
                *count += 1;
                return Some(Permit { hosts: self, key });
            }
            active = self
                .changed
                .wait_timeout(active, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self
            .hosts
            .active
            .lock()
            .unwrap()
            .get_mut(&self.key)
            .unwrap() -= 1;
        self.hosts.changed.notify_all();
    }
}
pub fn host(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if scheme == "file" {
            return "local".into();
        }
        let authority = rest
            .split('/')
            .next()
            .unwrap_or("")
            .rsplit('@')
            .next()
            .unwrap_or("");
        return if authority.starts_with('[') {
            authority
                .split(']')
                .next()
                .unwrap_or(authority)
                .to_ascii_lowercase()
        } else {
            authority
                .split(':')
                .next()
                .unwrap_or(authority)
                .to_ascii_lowercase()
        };
    }
    if let Some((left, _)) = url.split_once(':') {
        if !left.contains('/') {
            return left.rsplit('@').next().unwrap_or(left).to_ascii_lowercase();
        }
    }
    "local".into()
}
pub fn poll_interval(key: &str, seconds: u64) -> u64 {
    seconds.saturating_add(
        u64::from_str_radix(&crate::hash(key)[..8], 16).unwrap() % (seconds / 10 + 1),
    )
}
