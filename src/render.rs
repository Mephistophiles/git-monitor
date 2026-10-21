use crate::{
    config::DiffConfig,
    model::{FileDiff, Report, Row, Span},
};

const BRANCH_ICON: &str = r#"<svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M7 7v10m0-5h5a5 5 0 0 0 5-5"/><circle cx="7" cy="4" r="2.5"/><circle cx="7" cy="20" r="2.5"/><circle cx="17" cy="4" r="2.5"/></svg>"#;

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' => out.push('\t'),
            c if c.is_control() => out.push_str(&format!("⟨{:02X}⟩", c as u32)),
            _ => out.push(c),
        }
    }
    out
}
fn code(spans: &[Span]) -> String {
    let mut s = String::new();
    for p in spans {
        let class = format!(
            "token {} syntax-{}",
            if p.changed { "changed" } else { "" },
            p.syntax
        );
        s.push_str(&format!(
            "<span class=\"{class}\">{}</span>",
            escape(&p.text)
        ));
    }
    s
}
fn number(n: Option<usize>) -> String {
    n.map(|n| n.to_string()).unwrap_or_default()
}
fn row(r: &Row, split: bool) -> String {
    if r.kind == "hunk" {
        return format!(
            "<div class=\"hunk\">{}</div>",
            escape(&r.left.iter().map(|s| s.text.as_str()).collect::<String>())
        );
    }
    let moved = r
        .moved
        .map(|n| format!("<span class=\"move\" title=\"Moved line {n}\">↔{n}</span>"))
        .unwrap_or_default();
    if split {
        format!("<div class=\"diff-row {}\"><span class=\"ln\">{}</span><pre class=\"before\">{}{}</pre><span class=\"ln\">{}</span><pre class=\"after\">{}</pre></div>",r.kind,number(r.old),moved,code(&r.left),number(r.new),code(&r.right))
    } else {
        let mut out = String::new();
        if r.kind == "context" {
            out.push_str(&format!("<div class=\"unified-row context\"><span class=\"ln\">{}</span><span class=\"ln\">{}</span><span class=\"sign\"> </span><pre>{}</pre></div>",number(r.old),number(r.new),code(&r.right)));
        } else {
            if r.old.is_some() {
                out.push_str(&format!("<div class=\"unified-row delete\"><span class=\"ln\">{}</span><span class=\"ln\"></span><span class=\"sign\">−</span><pre>{}{}</pre></div>",number(r.old),moved,code(&r.left)));
            }
            if r.new.is_some() {
                out.push_str(&format!("<div class=\"unified-row insert\"><span class=\"ln\"></span><span class=\"ln\">{}</span><span class=\"sign\">+</span><pre>{}</pre></div>",number(r.new),code(&r.right)));
            }
        }
        out
    }
}
fn file(f: &FileDiff, index: usize, config: &DiffConfig) -> String {
    let adds = f
        .rows
        .iter()
        .filter(|r| r.kind == "insert" || r.kind == "replace")
        .count();
    let dels = f
        .rows
        .iter()
        .filter(|r| r.kind == "delete" || r.kind == "replace")
        .count();
    let source = if f.new_bytes.is_empty() {
        &f.old_bytes
    } else {
        &f.new_bytes
    };
    let mut out=format!("<section class=\"file\" id=\"file-{index}\" data-search=\"{}\"><header class=\"file-head\"><span class=\"file-icon\">⌘</span><strong>{}</strong><span class=\"language\">{}</span><span class=\"stats\"><b>+{adds}</b> <i>−{dels}</i></span><button class=\"copy\" data-copy=\"{}\" title=\"Copy source\">Copy</button><button class=\"copy\" data-export=\"{}\" title=\"Download exact source bytes\">↓</button></header>",escape(&f.path),escape(&f.path),escape(&f.language),source,source);
    if let Some(s) = &f.syntax {
        out.push_str(&format!("<div class=\"syntax-meta\"><span class=\"dot\"></span> Syntax-aware <span>· {} unchanged subtrees</span><span>· {} tokens removed / {} added</span>{}</div>",s.unchanged_subtrees,s.deleted_tokens,s.inserted_tokens,if s.text_only_change{"<span class=\"badge\">Formatting / comment change</span>"}else{""}));
    }
    if let Some(c) = &f.continuation {
        out.push_str(&format!("<div class=\"note\">Continuation {}/{} · old bytes {}..{} · new bytes {}..{} · source ranges are relative to this watch's selected content. Concatenate exports in part order; old SHA-256 {}; new SHA-256 {}.</div>",
            c.part,c.total,c.old_range.0,c.old_range.1,c.new_range.0,c.new_range.1,c.old_sha256,c.new_sha256));
        out.push_str(&format!("<button data-export=\"{}\" data-export-name=\"old-part-{}\">Download old range</button><button data-export=\"{}\" data-export-name=\"new-part-{}\">Download new range</button>",f.old_bytes,c.part,f.new_bytes,c.part));
    }
    if !f.note.is_empty() {
        out.push_str(&format!("<div class=\"note\">{}</div>", escape(&f.note)));
    }
    if f.rows.is_empty() {
        out.push_str(&format!(
            "<div class=\"empty-file\">{}</div>",
            escape(&f.kind)
        ));
    }
    for split in [false, true] {
        let cls = if split { "split-view" } else { "unified-view" };
        out.push_str(&format!("<div class=\"code-view {cls}\">"));
        for r in &f.rows {
            out.push_str(&row(r, split));
        }
        out.push_str("</div>");
    }
    if !config.line_numbers {
        out.push_str("<style>.ln{visibility:hidden}</style>");
    }
    out.push_str("</section>");
    out
}

pub fn html(report: &Report, config: &DiffConfig) -> String {
    let commits: usize = report.events.iter().map(|e| e.commits.len()).sum();
    let files: usize = report
        .events
        .iter()
        .map(|e| e.files.len() + e.commits.iter().map(|c| c.files.len()).sum::<usize>())
        .sum();
    let refs: std::collections::BTreeSet<_> = report
        .events
        .iter()
        .map(|e| format!("{}/{}", e.remote, e.branch))
        .collect();
    let mut nav = String::new();
    let mut body = String::new();
    let mut index = 0;
    for event in &report.events {
        let label = format!("{}/{}", event.remote, event.branch);
        nav.push_str(&format!(
            "<a href=\"#event-{}\"><span>{BRANCH_ICON}</span><div>{}<small>{}</small></div><em>{}</em></a>",
            event.id,
            escape(&event.repository),
            escape(&label),
            event.commits.len()
        ));
        body.push_str(&format!("<article class=\"event\" id=\"event-{}\"><div class=\"event-heading\"><span class=\"repo-tag\">{}</span><span class=\"branch\">{BRANCH_ICON} {}</span><span class=\"event-kind\">{}</span></div>",event.id,escape(&event.repository),escape(&label),escape(&event.kind.replace('_'," "))));
        if event.kind != "branch_updated" {
            body.push_str(&format!(
                "<div class=\"transition\"><code>{}</code><span>→</span><code>{}</code></div>",
                escape(
                    event
                        .old
                        .as_deref()
                        .unwrap_or("∅")
                        .get(..12)
                        .unwrap_or(event.old.as_deref().unwrap_or("∅"))
                ),
                escape(
                    event
                        .new
                        .as_deref()
                        .unwrap_or("∅")
                        .get(..12)
                        .unwrap_or(event.new.as_deref().unwrap_or("∅"))
                )
            ));
        }
        for note in &event.notes {
            body.push_str(&format!("<div class=\"note\">{}</div>", escape(note)));
        }
        for commit in &event.commits {
            body.push_str(&format!("<div class=\"commit-heading\"><span class=\"commit-dot\"></span><div><h2>{}</h2><p><code>{}</code><span>{}</span><span>{}</span></p></div></div>",escape(&commit.subject),escape(&commit.oid[..commit.oid.len().min(9)]),escape(&commit.author),escape(&event.watches.join(", "))));
            for f in &commit.files {
                index += 1;
                body.push_str(&file(f, index, config));
            }
        }
        for f in &event.files {
            index += 1;
            body.push_str(&file(f, index, config));
        }
        if !event.outgoing.is_empty() {
            body.push_str(&format!("<details class=\"outgoing\"><summary>{} commits removed from this ref</summary><pre>{}</pre></details>",event.outgoing.len(),escape(&event.outgoing.join("\n"))));
        }
        body.push_str("</article>");
    }
    if report.events.is_empty() {
        body.push_str("<section class=\"empty\"><h2>You're all caught up.</h2><p>No matching changes in this report.</p></section>");
    }
    let initial = if config.layouts == ["side-by-side"] {
        "split"
    } else {
        "unified"
    };
    format!(
        r##"<!doctype html><html lang="en" data-view="{initial}" data-theme="{}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'"><title>Git Monitor · {}</title><style>{}</style></head><body><aside><a class="brand" href="#top"><span class="brand-mark">g<span>m</span></span>git monitor<span class="version">0.1</span></a><div class="sidebar-label">YOUR DIGEST</div><div class="nav-title">Changes to follow</div><nav>{nav}</nav><div class="sidebar-bottom"><span class="live-dot"></span> Self-contained report<small>No server. No external requests.</small></div></aside><main id="top"><header class="topbar"><div><span class="eyebrow">ENGINEERING / CHANGE DIGEST</span><h1>Your code, in focus<span>.</span></h1><p>The changes that matter to you, across every branch and remote.</p></div><span class="offline"><span class="live-dot"></span>Offline ready</span></header><div class="overview"><div><span class="metric">{:02}</span><span>commits</span></div><div><span class="metric">{:02}</span><span>files changed</span></div><div><span class="metric">{:02}</span><span>tracked refs</span></div><div class="recipient"><small>PREPARED FOR</small><strong>{}</strong><span>Generated {}</span></div></div><section class="toolbar"><div class="view-buttons"><button data-view="unified">Unified</button><button data-view="split">Side by side</button></div><label class="search">⌕ <input type="search" placeholder="Find a file or change…" aria-label="Search changes"></label><button id="theme" title="Switch theme" aria-label="Switch theme">◐</button><span class="legend"><i></i>Added <b></b>Removed</span></section>{body}<footer>Generated by <strong>git-monitor</strong> · Native diff engine · Exact source exports · All data stays in this file</footer></main><div class="toast" role="status"></div><script>{}</script></body></html>"##,
        escape(&config.theme),
        escape(&report.recipient),
        include_str!("report.css"),
        commits,
        files,
        refs.len(),
        escape(&report.recipient),
        humantime::format_rfc3339(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(report.generated_at)
        ),
        include_str!("report.js")
    )
}
