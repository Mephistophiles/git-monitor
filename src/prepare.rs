//! Isolated HTML/MIME generation, with bounded time, address space and output.
use crate::{
    config::{Config, DiffConfig, Limits, Recipient},
    mail::{self, MailConfig, Smtp},
    model::Report,
    process::{self, Shutdown},
    render,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Task {
    #[serde(default)]
    pub render_cache: Option<std::path::PathBuf>,
    pub report: Report,
    pub diff: DiffConfig,
    pub limits: Limits,
    pub recipient: Option<Recipient>,
    pub from: Option<String>,
    pub max_message_bytes: usize,
}
#[derive(Serialize, Deserialize)]
pub struct Prepared {
    #[serde(default)]
    pub counters: crate::metrics::Counters,
    pub html: String,
    pub parts: Vec<(String, Vec<u8>, String)>,
}
fn dummy_mail(from: String) -> MailConfig {
    MailConfig {
        version: 1,
        smtp: Smtp {
            host: "localhost".into(),
            port: 0,
            security: "none".into(),
            username: None,
            password: None,
            password_env: None,
            from,
            timeout: "1s".into(),
        },
    }
}
pub fn execute(task: &Task) -> Result<Prepared> {
    render::set_cache(task.render_cache.clone());
    let mut parts = vec![];
    if let (Some(recipient), Some(from)) = (&task.recipient, &task.from) {
        let config = Config {
            version: 1,
            storage: crate::config::Storage {
                state_dir: Default::default(),
                repositories_dir: Default::default(),
            },
            execution: Default::default(),
            defaults: crate::config::Defaults {
                max_message_bytes: task.max_message_bytes,
                ..Default::default()
            },
            diff: task.diff.clone(),
            limits: task.limits.clone(),
            recipients: Default::default(),
            repositories: Default::default(),
            watches: vec![],
            maintenance: Default::default(),
        };
        parts = mail::package(&task.report, recipient, &dummy_mail(from.clone()), &config)?;
    }
    Ok(Prepared {
        counters: crate::metrics::snapshot(),
        html: render::html(&task.report, &task.diff),
        parts,
    })
}
pub fn bounded(
    cfg: &Config,
    report: &Report,
    recipient: Option<&Recipient>,
    from: Option<&str>,
    shutdown: &Shutdown,
) -> Result<Prepared> {
    let task = Task {
        render_cache: cfg
            .execution
            .share_computations
            .then(|| cfg.storage.state_dir.join("render-cache")),
        report: report.clone(),
        diff: cfg.diff.clone(),
        limits: cfg.limits.clone(),
        recipient: recipient.cloned(),
        from: from.map(String::from),
        max_message_bytes: cfg.defaults.max_message_bytes,
    };
    let directory = cfg.storage.state_dir.join("tmp");
    std::fs::create_dir_all(&directory)?;
    let input = tempfile::NamedTempFile::new_in(&directory)?;
    serde_json::to_writer(input.as_file(), &task)?;
    let output = tempfile::NamedTempFile::new_in(&directory)?;
    let run = process::run(
        std::process::Command::new(process::worker_executable()?)
            .arg("report-worker")
            .arg("--input")
            .arg(input.path())
            .arg("--output")
            .arg(output.path()),
        crate::config::duration(&cfg.limits.render_timeout)?,
        65536,
        shutdown,
        crate::config::duration(&cfg.execution.shutdown_grace)?,
        Some(cfg.limits.max_worker_address_space_bytes),
    );
    let prepared = run.and_then(|r| {
        ensure!(r.code == 0, "report worker failed or exceeded its budget");
        ensure!(
            std::fs::metadata(output.path())?.len() <= cfg.limits.max_render_output_bytes as u64,
            "render output budget exceeded"
        );
        Ok(serde_json::from_reader(std::fs::File::open(
            output.path(),
        )?)?)
    });
    if prepared.is_ok() || shutdown.requested() {
        return prepared;
    }
    let mut diagnostic = report.clone();
    diagnostic.events.truncate(1);
    for e in &mut diagnostic.events {
        e.commits.clear();
        e.files.clear();
        e.outgoing.clear();
        e.notes=vec!["oversized_item: report rendering/packaging exceeded a resource budget. Full scoped analysis remains in SQLite; raise limits and retry analysis.".into()];
    }
    let mut fallback = task;
    fallback.report = diagnostic;
    execute(&fallback)
}
