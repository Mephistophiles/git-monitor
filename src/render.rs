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
thread_local! {static CACHE:std::cell::RefCell<Option<std::path::PathBuf>>=const{std::cell::RefCell::new(None)};}
pub fn set_cache(path: Option<std::path::PathBuf>) {
    CACHE.with(|s| *s.borrow_mut() = path);
}
fn file(f: &FileDiff, index: usize, config: &DiffConfig) -> String {
    let key = crate::hash(serde_json::to_vec(&(crate::profile::render_key(), f, config)).unwrap());
    let directory = CACHE.with(|s| s.borrow().clone());
    if let Some(directory) = &directory {
        if let Some(bytes) = crate::cache::get(directory, &key, 256_000_000) {
            if let Ok(html) = String::from_utf8(bytes) {
                crate::metrics::update(|s| s.render_cache_hits += 1);
                return rebind(&html, index);
            }
        }
    }
    let html = file_uncached(f, 0, config);
    crate::metrics::update(|s| s.renders += 1);
    if let Some(directory) = directory {
        let _ = crate::cache::put(&directory, &key, html.as_bytes());
    }
    rebind(&html, index)
}
fn rebind(html: &str, index: usize) -> String {
    html.replace("id=\"file-0", &format!("id=\"file-{index}"))
        .replace("href=\"#file-0", &format!("href=\"#file-{index}"))
}
fn file_uncached(f: &FileDiff, index: usize, config: &DiffConfig) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let mut projected;
    let f = if let Some(document) = f.document.as_ref().filter(|d| !d.hunks.is_empty()) {
        if let (Ok(old), Ok(new)) = (STANDARD.decode(&f.old_bytes), STANDARD.decode(&f.new_bytes)) {
            if let Ok(rows) = crate::projection::rows(&document.hunks, &old, &new) {
                projected = f.clone();
                projected.rows = rows;
                &projected
            } else {
                f
            }
        } else {
            f
        }
    } else {
        f
    };
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
    out.push_str("<details class=\"file-body\" open><summary>Diff and source exports</summary>");
    if let Some(s) = &f.syntax {
        out.push_str(&format!("<div class=\"syntax-meta\"><span class=\"dot\"></span> Syntax-aware <span>· {} unchanged subtrees</span><span>· {} tokens removed / {} added</span>{}</div>",s.unchanged_subtrees,s.deleted_tokens,s.inserted_tokens,if s.text_only_change{"<span class=\"badge\">Formatting / comment change</span>"}else{""}));
    }
    if let Some(document) = &f.document {
        if !document.syntax_matches.is_empty() {
            out.push_str("<div class=\"structural-view\"><p>Syntax correspondences · all textual edits remain available</p><table><thead><tr><th>Relationship</th><th>Old bytes</th><th>New bytes</th><th>Evidence</th></tr></thead><tbody>");
            for m in &document.syntax_matches {
                out.push_str(&format!(
                    "<tr><td>{}</td><td>{}..{}</td><td>{}..{}</td><td>{}</td></tr>",
                    escape(&m.kind),
                    document.old.original_range.start + m.old.start,
                    document.old.original_range.start + m.old.end,
                    document.new.original_range.start + m.new.start,
                    document.new.original_range.start + m.new.end,
                    escape(&m.confidence)
                ));
            }
            out.push_str("</tbody></table></div>");
        }
    }
    if let Some(c) = &f.continuation {
        out.push_str(&format!("<div class=\"note\">Continuation {}/{} · old bytes {}..{} · new bytes {}..{} · source ranges are relative to this watch's selected content. Concatenate exports in part order; old SHA-256 {}; new SHA-256 {}.</div>",
            c.part,c.total,c.old_range.0,c.old_range.1,c.new_range.0,c.new_range.1,c.old_sha256,c.new_sha256));
        out.push_str(&format!("<button data-export=\"{}\" data-export-name=\"old-part-{}\">Download old range</button><button data-export=\"{}\" data-export-name=\"new-part-{}\">Download new range</button>",f.old_bytes,c.part,f.new_bytes,c.part));
    }
    if f.document.is_some() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let json = STANDARD.encode(serde_json::to_vec(f).expect("diff export"));
        out.push_str(&format!("<button data-export=\"{json}\" data-export-name=\"scoped-diff.json\">Download diff model</button>"));
    }
    if let Some(patch) = &f.canonical_patch {
        out.push_str(&format!("<button data-export=\"{patch}\" data-export-name=\"change.patch\">Download patch</button>"));
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
        for (n, r) in f.rows.iter().enumerate() {
            let id = format!("file-{index}-{cls}-row-{n}");
            out.push_str(&format!("<div id=\"{id}\" class=\"row-anchor\">"));
            if let Some(moved) = r.moved {
                if let Some((target, _)) = f
                    .rows
                    .iter()
                    .enumerate()
                    .find(|(j, other)| *j != n && other.moved == Some(moved))
                {
                    out.push_str(&format!("<a class=\"move-link\" href=\"#file-{index}-{cls}-row-{target}\">↔ move {moved}</a>"));
                }
            }
            if r.kind == "hunk" {
                out.push_str(&format!(
                    "<a class=\"hunk-link\" href=\"#{id}\" aria-label=\"Link to hunk\">#</a>"
                ));
            }
            out.push_str(&row(r, split));
            out.push_str("</div>");
        }
        out.push_str("</div>");
    }
    if !config.line_numbers {
        out.push_str("<style>.ln{visibility:hidden}</style>");
    }
    out.push_str("</details></section>");
    out
}

pub fn html(report: &Report, config: &DiffConfig) -> String {
    let commits = report
        .events
        .iter()
        .flat_map(|e| e.commits.iter().map(|c| (&e.repository, &c.oid)))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
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
    let mut seen = std::collections::BTreeSet::new();
    for event in &report.events {
        let label = format!("{}/{}", event.remote, event.branch);
        nav.push_str(&format!(
            "<a href=\"#event-{}\"><span>{BRANCH_ICON}</span><div>{}<small>{}</small></div><em>{}</em></a>",
            event.id,
            escape(&event.repository),
            escape(&label),
            event.commits.len()
        ));
        body.push_str(&format!("<article class=\"event\" data-kind=\"{}\" id=\"event-{}\"><div class=\"event-heading\"><span class=\"repo-tag\">{}</span><span class=\"branch\">{BRANCH_ICON} {}</span><span class=\"event-kind\">{}</span></div>",escape(&event.kind),event.id,escape(&event.repository),escape(&label),escape(&event.kind.replace('_'," "))));
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
        body.push_str("<div class=\"commit-comparison\">");
        for commit in &event.commits {
            let anchor = format!("commit-{}-{}", event.repository, commit.oid);
            if !seen.insert((&event.repository, &commit.oid)) {
                body.push_str(&format!(
                    "<p class=\"provenance\"><a href=\"#{}\">{}</a> · also observed in {}</p>",
                    escape(&anchor),
                    escape(&commit.subject),
                    escape(&label)
                ));
                continue;
            }
            body.push_str(&format!("<div id=\"{}\">", escape(&anchor)));
            body.push_str(&format!("<div class=\"commit-heading\"><span class=\"commit-dot\"></span><div><h2>{}</h2><p><code>{}</code><span>{}</span><span>{}</span></p></div></div>",escape(&commit.subject),escape(&commit.oid[..commit.oid.len().min(9)]),escape(&commit.author),escape(&event.watches.join(", "))));
            let provenance: Vec<_> = report
                .events
                .iter()
                .filter(|e| {
                    e.repository == event.repository
                        && e.commits.iter().any(|c| c.oid == commit.oid)
                })
                .map(|e| format!("{}/{}", e.remote, e.branch))
                .collect();
            body.push_str(&format!(
                "<p class=\"provenance\">Observed in: {}</p>",
                escape(&provenance.join(", "))
            ));
            let mut files = std::collections::BTreeSet::new();
            for matching in report
                .events
                .iter()
                .filter(|e| e.repository == event.repository)
                .flat_map(|e| e.commits.iter().filter(|c| c.oid == commit.oid))
            {
                for f in &matching.files {
                    if files.insert(crate::hash(serde_json::to_vec(f).unwrap())) {
                        index += 1;
                        body.push_str(&file(f, index, config));
                    }
                }
            }
            body.push_str("</div>");
        }
        body.push_str("</div><div class=\"aggregate-comparison\"><h3>Aggregate comparison</h3>");
        for f in &event.files {
            index += 1;
            body.push_str(&file(f, index, config));
        }
        body.push_str("</div>");
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
        r##"<!doctype html><html lang="en" data-layer="text" data-history="all" data-view="{initial}" data-theme="{}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'"><title>Git Monitor · {}</title><style>{}</style></head><body><aside><a class="brand" href="#top"><span class="brand-mark">g<span>m</span></span>git monitor<span class="version">0.1</span></a><div class="sidebar-label">YOUR DIGEST</div><div class="nav-title">Changes to follow</div><nav>{nav}</nav><div class="sidebar-bottom"><span class="live-dot"></span> Self-contained report<small>No server. No external requests.</small></div></aside><main id="top"><header class="topbar"><div><span class="eyebrow">ENGINEERING / CHANGE DIGEST</span><h1>Your code, in focus<span>.</span></h1><p>The changes that matter to you, across every branch and remote.</p></div><span class="offline"><span class="live-dot"></span>Offline ready</span></header><div class="overview"><div><span class="metric">{:02}</span><span>commits</span></div><div><span class="metric">{:02}</span><span>files changed</span></div><div><span class="metric">{:02}</span><span>tracked refs</span></div><div class="recipient"><small>PREPARED FOR</small><strong>{}</strong><span>Generated {}</span></div></div><section class="toolbar"><div class="view-buttons"><button data-view="unified">Unified</button><button data-view="split">Side by side</button></div><div class="view-buttons"><button data-layer="text">Text</button><button data-layer="structural">Structure</button></div><label>Events <select id="event-filter"><option value="all">All</option><option value="branch_updated">Commits</option><option value="history_rewritten">Rewrites</option><option value="branch_created">Created</option><option value="branch_deleted">Deleted</option></select></label><label>Comparison <select id="history-filter"><option value="all">All</option><option value="commits">Commits</option><option value="aggregate">Aggregate</option></select></label><label class="search">⌕ <input type="search" placeholder="Find a file or change…" aria-label="Search changes"></label><button id="theme" title="Switch theme" aria-label="Switch theme">◐</button><span class="legend"><i></i>Added <b></b>Removed</span></section>{body}<footer>Generated by <strong>git-monitor</strong> · Native diff engine · Exact source exports · All data stays in this file</footer></main><div class="toast" role="status"></div><script>{}</script></body></html>"##,
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
