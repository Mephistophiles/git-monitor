use git_monitor::{persistence, process::Shutdown, scheduler, store::Store};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
    time::Duration,
};
#[test]
fn bounded_pool_is_concurrent_and_merges_counts() {
    let barrier = Barrier::new(3);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    git_monitor::metrics::reset();
    let values = scheduler::map((0..6).collect(), 3, |i| {
        let n = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(n, Ordering::SeqCst);
        git_monitor::metrics::update(|c| c.git_processes += 1);
        barrier.wait();
        active.fetch_sub(1, Ordering::SeqCst);
        i * i
    });
    assert_eq!(values.len(), 6);
    assert_eq!(peak.load(Ordering::SeqCst), 3);
    assert_eq!(git_monitor::metrics::snapshot().git_processes, 6);
}
#[test]
fn host_slots_and_stable_jitter_bound_fetches() {
    let hosts = scheduler::Hosts::new(1);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    scheduler::map((0..6).collect(), 3, |_| {
        let _slot = hosts
            .acquire("ssh://user:secret@EXAMPLE.com/a", &Shutdown::default())
            .unwrap();
        let n = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(3));
        active.fetch_sub(1, Ordering::SeqCst);
    });
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    assert_eq!(scheduler::host("git@example.com:a"), "example.com");
    assert_eq!(scheduler::host("file:///tmp/a"), "local");
    assert_eq!(scheduler::host("/tmp/a"), "local");
    assert_eq!(scheduler::host("https://[::1]:80/a"), "[::1");
    let value = scheduler::poll_interval("key", 100);
    assert!((100..=110).contains(&value));
    assert_eq!(value, scheduler::poll_interval("key", 100));
}
#[test]
fn leases_fence_old_workers_and_recover_after_restart_or_expiry() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path()).unwrap();
    let first = store.claim_computation("key", &[], 100).unwrap();
    assert!(store.owns_computation("key", &first).unwrap());
    let second = store.claim_computation("key", &[], 100).unwrap();
    store.release_computation("key", &first, None).unwrap();
    assert!(!store.owns_computation("key", &first).unwrap());
    assert!(store.owns_computation("key", &second).unwrap());
    drop(store);
    let mut store = Store::open(temp.path()).unwrap();
    assert!(!store.owns_computation("key", &second).unwrap());
    let third = store.claim_computation("key", &[], 100).unwrap();
    store
        .db
        .execute("UPDATE computations SET expires=0", [])
        .unwrap();
    assert!(!store.owns_computation("key", &third).unwrap());
    store.jobs().unwrap();
    assert_eq!(
        store
            .db
            .query_row("SELECT state FROM computations", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "pending"
    );
}
#[test]
fn sqlite_commands_use_one_worker_and_propagate_errors() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path()).unwrap();
    let main = std::thread::current().id();
    let ids = persistence::scope(&mut store, |actor| {
        scheduler::map((0..12).collect(), 3, |i| {
            actor
                .call(move |s| {
                    s.db.execute(
                        "INSERT INTO maintenance_log(body) VALUES(?)",
                        [i.to_string()],
                    )?;
                    Ok(std::thread::current().id())
                })
                .unwrap()
        })
    });
    assert!(ids.iter().all(|id| *id != main && *id == ids[0]));
    persistence::scope(&mut store, |actor| {
        assert!(actor.call::<()>(|_| anyhow::bail!("failure")).is_err());
        assert_eq!(actor.call(|_| Ok(42)).unwrap(), 42);
    });
}
