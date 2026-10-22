use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Counters {
    pub git_processes: u64,
    pub blob_reads: u64,
    pub input_cache_hits: u64,
    pub input_cache_misses: u64,
    pub render_cache_hits: u64,
    pub renders: u64,
    pub analysis_cache_hits: u64,
    pub analyses: u64,
    pub child_cpu_user_seconds: f64,
    pub child_cpu_system_seconds: f64,
    pub max_waited_child_rss_bytes: Option<u64>,
}
thread_local! {static COUNTERS:std::cell::RefCell<Counters>=std::cell::RefCell::new(Counters::default());}
pub fn update(f: impl FnOnce(&mut Counters)) {
    COUNTERS.with(|c| f(&mut c.borrow_mut()));
}
pub fn reset() {
    COUNTERS.with(|c| *c.borrow_mut() = Counters::default());
}
pub fn merge(o: &Counters) {
    update(|s| {
        s.git_processes += o.git_processes;
        s.blob_reads += o.blob_reads;
        s.input_cache_hits += o.input_cache_hits;
        s.input_cache_misses += o.input_cache_misses;
        s.render_cache_hits += o.render_cache_hits;
        s.renders += o.renders;
        s.analysis_cache_hits += o.analysis_cache_hits;
        s.analyses += o.analyses;
    });
}
pub fn snapshot() -> Counters {
    let mut result = COUNTERS.with(|c| c.borrow().clone());
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) == 0 {
            result.child_cpu_user_seconds =
                usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1e6;
            result.child_cpu_system_seconds =
                usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1e6;
            #[cfg(target_os = "linux")]
            {
                result.max_waited_child_rss_bytes =
                    Some((usage.ru_maxrss as u64).saturating_mul(1024));
            }
            #[cfg(target_os = "macos")]
            {
                result.max_waited_child_rss_bytes = Some(usage.ru_maxrss as u64);
            }
        }
    }
    result
}
pub fn log(stage: &str) {
    eprintln!(
        "{}",
        serde_json::json!({"timestamp":crate::now(),"stage":stage,"counters":snapshot()})
    );
}
