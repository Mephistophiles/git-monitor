use crate::{
    config::{duration, Config, Recipient},
    model::{EventView, FileDiff, Report},
    render,
};
use anyhow::{ensure, Context, Result};
use lettre::{
    message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    Address, Message, SmtpTransport, Transport,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
pub struct MailConfig {
    pub version: u32,
    pub smtp: Smtp,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub password_env: Option<String>,
    pub from: String,
    pub timeout: String,
}
impl MailConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut c: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        ensure!(c.version == 1, "unsupported mail configuration version");
        ensure!(
            c.smtp.password.is_none() || c.smtp.password_env.is_none(),
            "use password or password_env, not both"
        );
        #[cfg(unix)]
        if c.smtp.password.is_some() {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(path)?.permissions().mode() & 0o077 == 0,
                "mail configuration with a password must be owner-only"
            );
        }
        if let Some(name) = c.smtp.password_env.take() {
            c.smtp.password = Some(
                std::env::var(&name).context("SMTP password environment variable is missing")?,
            );
        }
        ensure!(
            ["starttls", "tls", "none"].contains(&c.smtp.security.as_str()),
            "invalid SMTP security"
        );
        ensure!(
            c.smtp.username.is_some() == c.smtp.password.is_some(),
            "SMTP username and password must be set together"
        );
        c.smtp.from.parse::<Mailbox>()?;
        duration(&c.smtp.timeout)?;
        Ok(c)
    }
}

pub fn revision(c: &Config, id: &str, mail: &MailConfig) -> String {
    let sources: Vec<_> = c
        .watches
        .iter()
        .filter(|w| w.recipients.iter().any(|r| r == id))
        .flat_map(|w| {
            w.remotes.iter().map(move |remote| {
                (
                    &w.repository,
                    remote,
                    c.repositories
                        .get(&w.repository)
                        .and_then(|r| r.remotes.get(remote))
                        .map(|r| (&r.url, &r.access_context)),
                )
            })
        })
        .collect();
    crate::hash(
        serde_json::to_vec(&(
            crate::profile::render_key(),
            c.recipients.get(id),
            c.watches
                .iter()
                .filter(|w| w.recipients.iter().any(|r| r == id))
                .collect::<Vec<_>>(),
            &mail.smtp.from,
            &mail.smtp.host,
            mail.smtp.port,
            &mail.smtp.security,
            sources,
            &c.diff,
            c.defaults.max_message_bytes,
            c.limits.max_part_count,
            c.limits.max_render_output_bytes,
            &c.limits.render_timeout,
            c.limits.max_worker_address_space_bytes,
        ))
        .expect("mail revision"),
    )
}

pub fn message(
    report: &Report,
    recipient: &Recipient,
    mail: &MailConfig,
    config: &crate::config::DiffConfig,
    part: usize,
) -> Result<(String, Vec<u8>, String)> {
    let html = render::html(report, config);
    let id = format!(
        "<gm.{}@localhost>",
        crate::hash(serde_json::to_vec(&(
            report,
            &recipient.to,
            &mail.smtp.from,
            part,
            crate::hash(html.as_bytes())
        ))?)
    );
    let mut b = Message::builder()
        .from(mail.smtp.from.parse()?)
        .subject(format!("Git Monitor · {} · part {part}", report.recipient))
        .message_id(Some(id.clone()));
    for to in &recipient.to {
        b = b.to(to.parse()?);
    }
    let plain=format!("Git Monitor digest for {}\n{} observed ref transitions. Open the attached self-contained HTML report in a browser.\nGenerated at Unix timestamp {}.\n",report.recipient,report.events.len(),report.generated_at);
    let m=b.multipart(MultiPart::mixed().multipart(MultiPart::alternative().singlepart(SinglePart::plain(plain)).singlepart(SinglePart::html("<p>Your Git Monitor digest is attached as a self-contained HTML report.</p>".to_owned()))).singlepart(Attachment::new(format!("git-monitor-{part}.html")).body(html.clone(),ContentType::parse("text/html; charset=utf-8")?)))?;
    Ok((id, m.formatted(), html))
}

fn skeleton(e: &EventView) -> EventView {
    EventView {
        id: e.id,
        repository: e.repository.clone(),
        remote: e.remote.clone(),
        branch: e.branch.clone(),
        kind: e.kind.clone(),
        old: e.old.clone(),
        new: e.new.clone(),
        observed_at: e.observed_at,
        comparison_base: e.comparison_base.clone(),
        watches: e.watches.clone(),
        commits: vec![],
        files: vec![],
        notes: e.notes.clone(),
        outgoing: e.outgoing.clone(),
    }
}
fn fragments(report: &Report) -> Vec<EventView> {
    let mut out = vec![];
    for e in &report.events {
        for c in &e.commits {
            for f in &c.files {
                let mut event = skeleton(e);
                event.commits.push(crate::model::CommitView {
                    oid: c.oid.clone(),
                    subject: c.subject.clone(),
                    author: c.author.clone(),
                    files: vec![f.clone()],
                });
                out.push(event);
            }
        }
        for f in &e.files {
            let mut event = skeleton(e);
            event.files.push(f.clone());
            out.push(event);
        }
        if e.commits.is_empty() && e.files.is_empty() {
            out.push(skeleton(e));
        }
    }
    out
}
fn first_file(e: &mut EventView) -> Option<&mut FileDiff> {
    if let Some(c) = e.commits.first_mut() {
        c.files.first_mut()
    } else {
        e.files.first_mut()
    }
}
fn diagnostic(e: &EventView, reason: &str) -> EventView {
    let mut event = skeleton(e);
    event.notes.clear();
    event.outgoing.clear();
    event.files = vec![FileDiff::diagnostic("report", reason)];
    event
}

pub fn package(
    report: &Report,
    recipient: &Recipient,
    mail: &MailConfig,
    cfg: &Config,
) -> Result<Vec<(String, Vec<u8>, String)>> {
    let cap = cfg.defaults.max_message_bytes;
    let single = |event: EventView| Report {
        recipient: report.recipient.clone(),
        generated_at: report.generated_at,
        events: vec![event],
    };
    let mut expanded = vec![];
    for mut event in fragments(report) {
        if message(&single(event.clone()), recipient, mail, &cfg.diff, 1)?
            .1
            .len()
            <= cap
        {
            expanded.push(event);
            continue;
        }
        if let Some(f) = first_file(&mut event) {
            *f = crate::split::begin(f);
        }
        let mut todo = std::collections::VecDeque::from([event.clone()]);
        let mut pieces = vec![];
        let mut failed = false;
        while let Some(mut e) = todo.pop_front() {
            if message(&single(e.clone()), recipient, mail, &cfg.diff, 1)?
                .1
                .len()
                + 1024
                <= cap
            {
                pieces.push(e);
                continue;
            }
            let split = first_file(&mut e).and_then(|f| crate::split::bisect(f));
            if pieces.len() + todo.len() + 2 > cfg.limits.max_part_count || split.is_none() {
                failed = true;
                break;
            }
            let (left, right) = split.unwrap();
            let mut a = e.clone();
            *first_file(&mut a).unwrap() = left;
            *first_file(&mut e).unwrap() = right;
            todo.push_front(e);
            todo.push_front(a);
        }
        if failed {
            expanded.push(diagnostic(&event,"oversized_item: source/continuation cannot fit max_message_bytes and max_part_count. Full scoped results remain in SQLite; raise limits and retry analysis."));
        } else {
            let total = pieces.len();
            for (index, e) in pieces.iter_mut().enumerate() {
                if let Some(c) = first_file(e).and_then(|f| f.continuation.as_mut()) {
                    c.part = index + 1;
                    c.total = total;
                }
            }
            expanded.extend(pieces);
        }
    }
    let mut parts = vec![];
    let mut current = Report {
        recipient: report.recipient.clone(),
        generated_at: report.generated_at,
        events: vec![],
    };
    for event in expanded {
        let mut candidate = current.clone();
        candidate.events.push(event.clone());
        if message(&candidate, recipient, mail, &cfg.diff, parts.len() + 1)?
            .1
            .len()
            > cap
            && !current.events.is_empty()
        {
            parts.push(message(
                &current,
                recipient,
                mail,
                &cfg.diff,
                parts.len() + 1,
            )?);
            current.events.clear();
        }
        if parts.len() >= cfg.limits.max_part_count {
            parts.truncate(cfg.limits.max_part_count.saturating_sub(1));
            let card=single(diagnostic(&event,"oversized_item: max_part_count exceeded. Remaining scoped results are retained in SQLite; raise limits and retry analysis."));
            parts.push(message(&card, recipient, mail, &cfg.diff, parts.len() + 1)?);
            current.events.clear();
            break;
        }
        current.events.push(event);
    }
    if !current.events.is_empty() {
        parts.push(message(
            &current,
            recipient,
            mail,
            &cfg.diff,
            parts.len() + 1,
        )?);
    }
    ensure!(
        parts.len() <= cfg.limits.max_part_count && parts.iter().all(|p| p.1.len() <= cap),
        "MIME limit cannot fit report metadata/diagnostics"
    );
    Ok(parts)
}

pub fn smtp_send(smtp: &Smtp, to: &[String], bytes: &[u8]) -> Result<()> {
    let mut b = match smtp.security.as_str() {
        "starttls" => SmtpTransport::starttls_relay(&smtp.host)?,
        "tls" => SmtpTransport::relay(&smtp.host)?,
        "none" => SmtpTransport::builder_dangerous(&smtp.host),
        _ => anyhow::bail!("invalid SMTP security"),
    }
    .port(smtp.port)
    .timeout(Some(duration(&smtp.timeout)?));
    if let (Some(u), Some(p)) = (&smtp.username, &smtp.password) {
        b = b.credentials(Credentials::new(u.clone(), p.clone()));
    }
    let from = smtp.from.parse::<Mailbox>()?.email;
    let recipients = to
        .iter()
        .map(|s| Ok(s.parse::<Mailbox>()?.email))
        .collect::<Result<Vec<Address>>>()?;
    let envelope = lettre::address::Envelope::new(Some(from), recipients)?;
    b.build()
        .send_raw(&envelope, bytes)
        .map_err(|_| anyhow::anyhow!("SMTP delivery failed; the outcome may be ambiguous"))?;
    Ok(())
}
