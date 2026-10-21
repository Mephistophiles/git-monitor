use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use git_monitor::{
    config::{duration, Config},
    mail::{self, MailConfig},
    monitor::{self, Locks},
    process::Shutdown,
    store::{Analysis, Store, Task},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    version,
    about = "Monitor Git remotes and produce scoped offline HTML digests"
)]
struct Cli {
    #[arg(long, global = true, default_value = "monitor.toml")]
    config: PathBuf,
    #[arg(long, global = true)]
    mail_config: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    CheckConfig,
    Run {
        #[arg(long)]
        once: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Prepare the MIME outbox without connecting to SMTP.
        #[arg(long)]
        prepare_only: bool,
    },
    Status,
    Preview {
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        output: PathBuf,
    },
    Doctor,
    RetryDeliveries,
    Maintain {
        #[arg(long)]
        gc: bool,
    },
    RetryAnalysis {
        #[arg(long)]
        job: i64,
    },
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    #[command(hide = true)]
    SmtpWorker {
        #[arg(long)]
        message: PathBuf,
    },
    #[command(hide = true)]
    ReportWorker {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}
fn main() {
    if let Err(e) = entry() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    if let Action::ReportWorker { input, output } = &cli.command {
        std::env::set_var("GIT_MONITOR_WORKER", "1");
        let task: git_monitor::prepare::Task = serde_json::from_slice(&std::fs::read(input)?)?;
        let result = git_monitor::prepare::execute(&task)?;
        let bytes = serde_json::to_vec(&result)?;
        anyhow::ensure!(
            bytes.len() <= task.limits.max_render_output_bytes,
            "render output budget exceeded"
        );
        git_monitor::atomic_write(output, &bytes)?;
        return Ok(());
    }
    if let Action::Worker { input, output } = &cli.command {
        std::env::set_var("GIT_MONITOR_WORKER", "1");
        let task: Task = serde_json::from_slice(&std::fs::read(input)?)?;
        let result: Analysis = monitor::analyze(&task)?;
        let bytes = serde_json::to_vec(&result)?;
        anyhow::ensure!(
            bytes.len() <= task.limits.max_job_output_bytes,
            "analysis output budget exceeded"
        );
        git_monitor::atomic_write(output, &bytes)?;
        return Ok(());
    }
    if let Action::SmtpWorker { message } = &cli.command {
        let smtp: mail::Smtp = serde_json::from_str(&std::env::var("GIT_MONITOR_SMTP")?)?;
        let to: Vec<String> = serde_json::from_str(&std::env::var("GIT_MONITOR_TO")?)?;
        return mail::smtp_send(&smtp, &to, &std::fs::read(message)?);
    }
    let mut cfg = Config::load(&cli.config)?;
    let mail = cli
        .mail_config
        .as_ref()
        .map(|p| MailConfig::load(p))
        .transpose()?;
    if matches!(cli.command, Action::CheckConfig) {
        println!(
            "Configuration valid: {} repositories, {} watches, {} recipients",
            cfg.repositories.len(),
            cfg.watches.len(),
            cfg.recipients.len()
        );
        return Ok(());
    }
    if matches!(cli.command, Action::Doctor) {
        println!("{}", git_monitor::doctor::check(&cfg)?);
        return Ok(());
    }
    cfg.freeze()?;
    let _locks = Locks::acquire(&cfg)?;
    let mut store = Store::open(&cfg.storage.state_dir)?;
    let shutdown = Shutdown::install()?;
    match cli.command {
        Action::Maintain { gc } => println!(
            "{}",
            serde_json::to_string(&git_monitor::maintenance::run(
                &cfg, &mut store, &shutdown, gc
            )?)?
        ),
        Action::Status => println!("{}", store.status()?),
        Action::Preview { recipient, output } => {
            let (report, _) = monitor::report(&cfg, &store, &recipient, false)?;
            git_monitor::atomic_write(
                &output,
                git_monitor::prepare::bounded(&cfg, &report, None, None, &shutdown)?
                    .html
                    .as_bytes(),
            )?;
            println!("{}", output.display());
        }
        Action::RetryAnalysis { job } => {
            store.retry_job(job)?;
            println!("Analysis job {job} queued");
        }
        Action::RetryDeliveries => {
            let mail = mail.context("--mail-config is required")?;
            monitor::recover(&mut store, &shutdown)?;
            println!(
                "{} messages sent",
                monitor::send(&cfg, &store, &mail, &shutdown, true)?
            );
        }
        Action::Run {
            once,
            force,
            output_dir,
            prepare_only,
        } => {
            monitor::doctor()?;
            for id in cfg.recipients.keys() {
                store.clock(id)?;
            }
            let mut first = true;
            loop {
                let (events, failures) =
                    monitor::collect(&cfg, &mut store, &shutdown, force && first)?;
                let analyses = monitor::process_jobs(&cfg, &mut store, &shutdown)?;
                let queued = if !shutdown.requested() {
                    monitor::outputs(
                        &cfg,
                        &mut store,
                        mail.as_ref(),
                        &shutdown,
                        output_dir.as_deref(),
                    )?
                } else {
                    0
                };
                let sent = if let Some(mail) = mail.as_ref().filter(|_| !prepare_only) {
                    monitor::send(&cfg, &store, mail, &shutdown, false)?
                } else {
                    0
                };
                println!("events={events} analyses={analyses} queued={queued} sent={sent} fetch_failures={failures}");
                if once || shutdown.requested() {
                    if failures > 0 && !shutdown.requested() {
                        anyhow::bail!(
                            "one or more remotes failed; successful sources were retained"
                        );
                    }
                    break;
                }
                first = false;
                let start = Instant::now();
                let delay = duration(&cfg.defaults.poll_every)?.min(Duration::from_secs(1));
                while start.elapsed() < delay && !shutdown.requested() {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
