use crate::{
    config::{DiffConfig, Limits},
    model::{FileDiff, Row, Span},
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
    use unicode_segmentation::UnicodeSegmentation;
    let a: Vec<_> = old.graphemes(true).collect();
    let b: Vec<_> = new.graphemes(true).collect();
    let mut left = vec![];
    let mut right = vec![];
    for op in similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &a,
        &b,
        Some(Instant::now() + std::time::Duration::from_millis(100)),
    ) {
        let (tag, x, y) = op.as_tag_tuple();
        if !x.is_empty() {
            left.extend(span(&a[x].concat(), tag != similar::DiffTag::Equal));
        }
        if !y.is_empty() {
            right.extend(span(&b[y].concat(), tag != similar::DiffTag::Equal));
        }
    }
    (left, right)
}

fn pairing(left: &[Row], right: &[Row], limit: usize) -> Vec<(Option<usize>, Option<usize>)> {
    let n = left.len();
    let m = right.len();
    if n.saturating_mul(m) > limit {
        return (0..n)
            .map(|i| (Some(i), None))
            .chain((0..m).map(|i| (None, Some(i))))
            .collect();
    }
    let tokens = |r: &Row, old: bool| {
        let text: String = (if old { &r.left } else { &r.right })
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect::<BTreeSet<_>>()
    };
    let a: Vec<_> = left.iter().map(|r| tokens(r, true)).collect();
    let b: Vec<_> = right.iter().map(|r| tokens(r, false)).collect();
    let score = |i: usize, j: usize| {
        let total = a[i].len() + b[j].len();
        if total == 0 {
            1.0
        } else {
            2.0 * a[i].intersection(&b[j]).count() as f32 / total as f32
        }
    };
    let mut dp = vec![0f32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let matched = score(i, j) - 0.3;
            dp[i * (m + 1) + j] =
                dp[(i + 1) * (m + 1) + j]
                    .max(dp[i * (m + 1) + j + 1])
                    .max(if matched > 0.0 {
                        dp[(i + 1) * (m + 1) + j + 1] + matched
                    } else {
                        0.0
                    });
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut result = vec![];
    while i < n || j < m {
        if i < n
            && j < m
            && score(i, j) > 0.3
            && dp[i * (m + 1) + j] <= dp[(i + 1) * (m + 1) + j + 1] + score(i, j) - 0.3 + 0.00001
        {
            result.push((Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if i < n && (j == m || dp[(i + 1) * (m + 1) + j] >= dp[i * (m + 1) + j + 1]) {
            result.push((Some(i), None));
            i += 1;
        } else {
            result.push((None, Some(j)));
            j += 1;
        }
    }
    result
}

pub fn text_rows(
    old: &str,
    new: &str,
    old_start: usize,
    new_start: usize,
    config: &DiffConfig,
    limits: &Limits,
) -> Vec<Row> {
    byte_rows(
        old.as_bytes(),
        new.as_bytes(),
        old_start,
        new_start,
        config,
        limits,
    )
}
fn byte_rows(
    old: &[u8],
    new: &[u8],
    old_start: usize,
    new_start: usize,
    config: &DiffConfig,
    limits: &Limits,
) -> Vec<Row> {
    let ranges = |bytes: &[u8]| {
        let mut result = vec![];
        let mut start = 0;
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                result.push(start..i + 1);
                start = i + 1;
            }
        }
        if start < bytes.len() {
            result.push(start..bytes.len());
        }
        result
    };
    let (ar, br) = (ranges(old), ranges(new));
    let aa: Vec<&[u8]> = ar.iter().map(|r| &old[r.clone()]).collect();
    let bb: Vec<&[u8]> = br.iter().map(|r| &new[r.clone()]).collect();
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_secs(2))
        .diff_slices(&aa, &bb);
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
                let value =
                    String::from_utf8_lossy(c.value().strip_suffix(b"\n").unwrap_or(c.value()));
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
                        span(&value, false)
                    } else {
                        vec![]
                    },
                    right: if c.tag() != ChangeTag::Delete {
                        span(&value, false)
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
                s.get(1..)
                    .unwrap_or("")
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
        let bounded = (split - begin).saturating_mul(i - split) <= limits.max_alignment_cells;
        if !bounded {
            out.push(Row {
                kind: "hunk".into(),
                old: None,
                new: None,
                left: span("alignment_budget_exceeded: original edits retained", false),
                right: vec![],
                moved: None,
            });
        }
        for (a, b) in pairing(
            &rows[begin..split],
            &rows[split..i],
            limits.max_alignment_cells,
        ) {
            match (a, b) {
                (Some(a), Some(b)) => {
                    let a = &rows[begin + a];
                    let b = &rows[split + b];
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
                }
                (Some(a), None) => out.push(rows[begin + a].clone()),
                (None, Some(b)) => out.push(rows[split + b].clone()),
                _ => unreachable!(),
            }
        }
    }
    if config.moved_lines {
        let mut blocks: BTreeMap<String, Vec<Vec<usize>>> = BTreeMap::new();
        let mut cursor = 0;
        while cursor < out.len() {
            if out[cursor].kind != "delete" {
                cursor += 1;
                continue;
            }
            let start = cursor;
            while cursor < out.len() && out[cursor].kind == "delete" {
                cursor += 1;
            }
            let indices: Vec<_> = (start..cursor).collect();
            let text = indices
                .iter()
                .map(|&i| {
                    out[i]
                        .left
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.trim().len() > 3 {
                blocks.entry(text).or_default().push(indices);
            }
        }
        let mut id = 0;
        cursor = 0;
        while cursor < out.len() {
            if out[cursor].kind != "insert" {
                cursor += 1;
                continue;
            }
            let start = cursor;
            while cursor < out.len() && out[cursor].kind == "insert" {
                cursor += 1;
            }
            let text = (start..cursor)
                .map(|i| {
                    out[i]
                        .right
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(candidates) = blocks.get_mut(&text) {
                if !candidates.is_empty() {
                    let indices = candidates.remove(0);
                    id += 1;
                    for i in indices.into_iter().chain(start..cursor) {
                        out[i].moved = Some(id);
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn grammar(path: &str) -> Option<(tree_sitter::Language, &'static str)> {
    match path.rsplit('.').next()? {
        "c" | "h" => Some((tree_sitter_c::LANGUAGE.into(), "C")),
        "cpp" | "cc" | "hpp" | "cxx" | "hh" => Some((tree_sitter_cpp::LANGUAGE.into(), "C++")),
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), "Rust")),
        "py" => Some((tree_sitter_python::LANGUAGE.into(), "Python")),
        "json" => Some((tree_sitter_json::LANGUAGE.into(), "JSON")),
        _ => None,
    }
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
    let mut note = String::new();
    let mut engine = "text".to_owned();
    let mut summary = None;
    let mut matches = vec![];
    let mut language = grammar(path).map(|(_, s)| s.to_owned()).unwrap_or_default();
    if config.mode != "text" {
        match crate::structural::compare(path, old, new, limits) {
            Ok((s, name, syntax_matches)) => {
                matches = syntax_matches;
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
        None => byte_rows(old, new, old_start, new_start, config, limits),
    };
    if patch.is_some() && (old_start != 1 || new_start != 1) {
        for row in &mut rows {
            row.old = row.old.map(|n| n + old_start - 1);
            row.new = row.new.map(|n| n + new_start - 1);
            if row.kind == "hunk" {
                for span in &mut row.left {
                    let fields: Vec<_> = span.text.split_whitespace().collect();
                    if fields.len() >= 3 {
                        let coord = |token: &str, offset: usize| {
                            let mut parts = token[1..].split(',');
                            let n = parts.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
                            format!(
                                "{}{},{}",
                                &token[..1],
                                if n == 0 { 0 } else { n + offset },
                                parts.next().unwrap_or("1")
                            )
                        };
                        span.text = format!(
                            "@@ {} {} @@ scoped function",
                            coord(fields[1], old_start - 1),
                            coord(fields[2], new_start - 1)
                        );
                    }
                }
            }
        }
    }
    if !old.is_empty() && !old.ends_with(b"\n") {
        note.push_str(" · old source has no final newline");
    }
    if !new.is_empty() && !new.ends_with(b"\n") {
        note.push_str(" · new source has no final newline");
    }
    apply_highlights(path, old, new, old_start, new_start, &mut rows, limits);
    let mut document = crate::document::DiffDocument::build(
        path,
        old,
        new,
        old_start,
        new_start,
        limits.max_alignment_cells,
    );
    document.hunks = match crate::projection::project(&rows, old, new, old_start, new_start) {
        Ok(hunks) => hunks,
        Err(_) => {
            document
                .quality
                .push("presentation_projection_fallback".into());
            rows = byte_rows(old, new, old_start, new_start, config, limits);
            apply_highlights(path, old, new, old_start, new_start, &mut rows, limits);
            crate::projection::project(&rows, old, new, old_start, new_start)
                .expect("selected source coordinates")
        }
    };
    document.syntax_matches = matches;
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
        continuation: None,
        document: Some(document),
        canonical_patch: patch.map(|p| STANDARD.encode(p)),
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
