use crate::{
    config::{duration, DiffConfig, Limits},
    model::{FileDiff, Row, Span, SyntaxSummary},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use similar::{ChangeTag, TextDiff};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

fn span(text: &str, changed: bool) -> Vec<Span> {
    vec![Span {
        text: text.to_owned(),
        changed,
        syntax: String::new(),
    }]
}
fn inline(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_millis(100))
        .diff_chars(old, new);
    let (mut left, mut right) = (vec![], vec![]);
    for c in diff.iter_all_changes() {
        if c.tag() != ChangeTag::Insert {
            left.push(Span {
                text: c.value().to_owned(),
                changed: c.tag() != ChangeTag::Equal,
                syntax: String::new(),
            });
        }
        if c.tag() != ChangeTag::Delete {
            right.push(Span {
                text: c.value().to_owned(),
                changed: c.tag() != ChangeTag::Equal,
                syntax: String::new(),
            });
        }
    }
    (left, right)
}

pub fn text_rows(
    old: &str,
    new: &str,
    old_start: usize,
    new_start: usize,
    config: &DiffConfig,
    limits: &Limits,
) -> Vec<Row> {
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_secs(2))
        .diff_lines(old, new);
    let mut rows = vec![];
    for group in diff.grouped_ops(config.context_lines) {
        rows.push(Row {
            kind: "hunk".into(),
            old: None,
            new: None,
            left: span("", false),
            right: vec![],
            moved: None,
        });
        for op in group {
            for c in diff.iter_changes(&op) {
                let value = c.value().trim_end_matches('\n');
                rows.push(Row {
                    kind: match c.tag() {
                        ChangeTag::Equal => "context",
                        ChangeTag::Delete => "delete",
                        ChangeTag::Insert => "insert",
                    }
                    .into(),
                    old: c.old_index().map(|i| i + old_start),
                    new: c.new_index().map(|i| i + new_start),
                    left: if c.tag() != ChangeTag::Insert {
                        span(value, false)
                    } else {
                        vec![]
                    },
                    right: if c.tag() != ChangeTag::Delete {
                        span(value, false)
                    } else {
                        vec![]
                    },
                    moved: None,
                });
            }
        }
    }
    align(rows, config, limits)
}

pub fn patch_rows(patch: &[u8], config: &DiffConfig, limits: &Limits) -> Vec<Row> {
    let mut rows = vec![];
    let (mut old, mut new) = (0, 0);
    let mut hunk = false;
    for line in String::from_utf8_lossy(patch).split_terminator('\n') {
        if line.starts_with("@@ ") {
            let fields: Vec<_> = line.split_whitespace().collect();
            let number = |s: &str| {
                s[1..]
                    .split(',')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0)
            };
            if fields.len() >= 3 {
                old = number(fields[1]);
                new = number(fields[2]);
                hunk = true;
            }
            rows.push(Row {
                kind: "hunk".into(),
                old: None,
                new: None,
                left: span(line, false),
                right: vec![],
                moved: None,
            });
            continue;
        }
        if !hunk {
            continue;
        }
        let (kind, a, b) = match line.as_bytes().first() {
            Some(b' ') => ("context", true, true),
            Some(b'-') => ("delete", true, false),
            Some(b'+') => ("insert", false, true),
            _ => continue,
        };
        let value = &line[1..];
        rows.push(Row {
            kind: kind.into(),
            old: a.then_some(old),
            new: b.then_some(new),
            left: if a { span(value, false) } else { vec![] },
            right: if b { span(value, false) } else { vec![] },
            moved: None,
        });
        if a {
            old += 1;
        }
        if b {
            new += 1;
        }
    }
    align(rows, config, limits)
}

fn align(rows: Vec<Row>, config: &DiffConfig, limits: &Limits) -> Vec<Row> {
    let mut out = vec![];
    let mut i = 0;
    while i < rows.len() {
        if rows[i].kind != "delete" {
            out.push(rows[i].clone());
            i += 1;
            continue;
        }
        let begin = i;
        while i < rows.len() && rows[i].kind == "delete" {
            i += 1;
        }
        let split = i;
        while i < rows.len() && rows[i].kind == "insert" {
            i += 1;
        }
        let count = (split - begin).min(i - split);
        let bounded = (split - begin).saturating_mul(i - split) <= limits.max_alignment_cells;
        for n in 0..(split - begin).max(i - split) {
            if n < count && bounded {
                let a = &rows[begin + n];
                let b = &rows[split + n];
                let old: String = a.left.iter().map(|s| s.text.as_str()).collect();
                let new: String = b.right.iter().map(|s| s.text.as_str()).collect();
                let (left, right) =
                    if old.len().saturating_mul(new.len()) <= limits.max_alignment_cells {
                        inline(&old, &new)
                    } else {
                        (span(&old, true), span(&new, true))
                    };
                out.push(Row {
                    kind: "replace".into(),
                    old: a.old,
                    new: b.new,
                    left,
                    right,
                    moved: None,
                });
            } else {
                if n < split - begin {
                    out.push(rows[begin + n].clone());
                }
                if n < i - split {
                    out.push(rows[split + n].clone());
                }
            }
        }
    }
    if config.moved_lines {
        let mut deleted: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, r) in out.iter().enumerate() {
            if r.kind == "delete" {
                let s: String = r.left.iter().map(|s| s.text.as_str()).collect();
                if s.trim().len() > 3 {
                    deleted.entry(s).or_default().push(i);
                }
            }
        }
        let mut id = 0;
        for i in 0..out.len() {
            if out[i].kind == "insert" {
                let s: String = out[i].right.iter().map(|s| s.text.as_str()).collect();
                if let Some(indices) = deleted.get_mut(&s) {
                    if let Some(j) = indices.pop() {
                        id += 1;
                        out[j].moved = Some(id);
                        out[i].moved = Some(id);
                    }
                }
            }
        }
    }
    out
}

fn grammar(path: &str) -> Option<(tree_sitter::Language, &'static str)> {
    match path.rsplit('.').next()? {
        "c" | "h" => Some((tree_sitter_c::LANGUAGE.into(), "C")),
        "cpp" | "cc" | "hpp" | "cxx" | "hh" => Some((tree_sitter_cpp::LANGUAGE.into(), "C++")),
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), "Rust")),
        "py" => Some((tree_sitter_python::LANGUAGE.into(), "Python")),
        "json" => Some((tree_sitter_json::LANGUAGE.into(), "JSON")),
        _ => None,
    }
}

fn syntax(
    path: &str,
    old: &[u8],
    new: &[u8],
    limits: &Limits,
) -> Result<(SyntaxSummary, String), String> {
    let (language, name) = grammar(path).ok_or("unsupported_language")?;
    if std::str::from_utf8(old).is_err() || std::str::from_utf8(new).is_err() {
        return Err("invalid_utf8".into());
    }
    let start = Instant::now();
    let timeout = duration(&limits.syntax_timeout).map_err(|_| "syntax_budget_exceeded")?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| "grammar_error")?;
    #[allow(deprecated)]
    parser.set_timeout_micros(timeout.as_micros().min(u64::MAX as u128) as u64);
    let mut sides = vec![];
    for source in [old, new] {
        let tree = parser.parse(source, None).ok_or("syntax_budget_exceeded")?;
        if tree.root_node().has_error() {
            return Err("parse_error".into());
        }
        let mut stack = vec![tree.root_node()];
        let mut count = 0;
        let mut tokens = vec![];
        let mut hashes = BTreeSet::new();
        while let Some(node) = stack.pop() {
            count += 1;
            if count > limits.max_syntax_nodes || start.elapsed() > timeout {
                return Err("syntax_budget_exceeded".into());
            }
            if node.kind().contains("comment") {
                continue;
            }
            if node.child_count() == 0 {
                tokens.push(format!(
                    "{}:{}",
                    node.kind(),
                    String::from_utf8_lossy(&source[node.byte_range()])
                ));
            } else {
                if node.is_named() && node.end_byte() - node.start_byte() <= 8192 {
                    hashes.insert(crate::hash(&source[node.byte_range()]));
                }
                for n in (0..node.child_count()).rev() {
                    if let Some(c) = node.child(n) {
                        stack.push(c);
                    }
                }
            }
        }
        sides.push((tokens, hashes));
    }
    if sides[0].0.len().saturating_mul(sides[1].0.len()) > limits.max_syntax_candidates {
        return Err("syntax_budget_exceeded".into());
    }
    let a: Vec<_> = sides[0].0.iter().map(String::as_str).collect();
    let b: Vec<_> = sides[1].0.iter().map(String::as_str).collect();
    let ops = similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &a,
        &b,
        Some(start + timeout),
    );
    let (mut deleted, mut inserted) = (0, 0);
    for op in ops {
        let (tag, old_range, new_range) = op.as_tag_tuple();
        if tag != similar::DiffTag::Equal {
            deleted += old_range.len();
            inserted += new_range.len();
        }
    }
    Ok((
        SyntaxSummary {
            unchanged_subtrees: sides[0].1.intersection(&sides[1].1).count(),
            deleted_tokens: deleted,
            inserted_tokens: inserted,
            text_only_change: a == b && old != new,
        },
        name.into(),
    ))
}

// The two source buffers carry independent original-file coordinates.
#[allow(clippy::too_many_arguments)]
pub fn build(
    path: &str,
    old: &[u8],
    new: &[u8],
    old_start: usize,
    new_start: usize,
    patch: Option<&[u8]>,
    config: &DiffConfig,
    limits: &Limits,
) -> FileDiff {
    let a = String::from_utf8_lossy(old);
    let b = String::from_utf8_lossy(new);
    let mut note = String::new();
    let mut engine = "text".to_owned();
    let mut summary = None;
    let mut language = grammar(path).map(|(_, s)| s.to_owned()).unwrap_or_default();
    if config.mode != "text" {
        match syntax(path, old, new, limits) {
            Ok((s, name)) => {
                engine = "syntax".into();
                language = name;
                summary = Some(s);
            }
            Err(e) => note = e,
        }
    }
    if std::str::from_utf8(old).is_err() || std::str::from_utf8(new).is_err() {
        note.push_str(" · display uses replacement characters; exports preserve exact bytes");
    }
    let mut rows = match patch {
        Some(p) => patch_rows(p, config, limits),
        None => text_rows(&a, &b, old_start, new_start, config, limits),
    };
    apply_highlights(path, old, new, old_start, new_start, &mut rows, limits);
    FileDiff {
        path: path.into(),
        kind: if old.is_empty() {
            "added"
        } else if new.is_empty() {
            "deleted"
        } else {
            "modified"
        }
        .into(),
        engine,
        note,
        rows,
        old_bytes: STANDARD.encode(old),
        new_bytes: STANDARD.encode(new),
        old_start,
        new_start,
        language,
        syntax: summary,
    }
}

fn apply_highlights(
    path: &str,
    old: &[u8],
    new: &[u8],
    old_start: usize,
    new_start: usize,
    rows: &mut [Row],
    limits: &Limits,
) {
    let Some((language, _)) = grammar(path) else {
        return;
    };
    for (source, start, left) in [(old, old_start, true), (new, new_start, false)] {
        if std::str::from_utf8(source).is_err() {
            continue;
        }
        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&language).is_err() {
            continue;
        }
        #[allow(deprecated)]
        parser.set_timeout_micros(100_000);
        let Some(tree) = parser.parse(source, None) else {
            continue;
        };
        let mut stack = vec![tree.root_node()];
        let mut ranges = Vec::new();
        let mut count = 0;
        while let Some(n) = stack.pop() {
            count += 1;
            if count > limits.max_syntax_nodes {
                break;
            }
            let kind = n.kind();
            let class = if kind.contains("comment") {
                "comment"
            } else if kind.contains("string") || kind.contains("char_literal") {
                "string"
            } else if kind.contains("number")
                || kind.contains("integer_literal")
                || kind.contains("float_literal")
            {
                "number"
            } else if kind.contains("type") && n.child_count() == 0 {
                "type"
            } else if !n.is_named() && source[n.byte_range()].iter().all(u8::is_ascii_alphabetic) {
                "keyword"
            } else {
                ""
            };
            if !class.is_empty() {
                ranges.push((n.start_byte(), n.end_byte(), class));
            } else {
                for i in (0..n.child_count()).rev() {
                    if let Some(c) = n.child(i) {
                        stack.push(c);
                    }
                }
            }
        }
        ranges.sort_by_key(|r| r.0);
        let mut lines = vec![0];
        for (i, b) in source.iter().enumerate() {
            if *b == b'\n' {
                lines.push(i + 1);
            }
        }
        for row in rows.iter_mut() {
            let Some(number) = (if left { row.old } else { row.new }) else {
                continue;
            };
            let Some(&base) = number.checked_sub(start).and_then(|n| lines.get(n)) else {
                continue;
            };
            let spans = if left { &mut row.left } else { &mut row.right };
            let mut offset = base;
            let mut annotated = Vec::new();
            for span in spans.iter() {
                for (i, c) in span.text.char_indices() {
                    let pos = ranges.partition_point(|r| r.0 <= offset + i);
                    let class = pos
                        .checked_sub(1)
                        .and_then(|p| ranges.get(p))
                        .filter(|r| offset + i < r.1)
                        .map(|r| r.2)
                        .unwrap_or("");
                    if let Some(last) = annotated.last_mut() {
                        let last: &mut Span = last;
                        if last.changed == span.changed && last.syntax == class {
                            last.text.push(c);
                            continue;
                        }
                    }
                    annotated.push(Span {
                        text: c.to_string(),
                        changed: span.changed,
                        syntax: class.into(),
                    });
                }
                offset += span.text.len();
            }
            *spans = annotated;
        }
    }
}
