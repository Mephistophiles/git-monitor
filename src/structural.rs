//! Byte-scoped syntax correspondences; textual edits remain complete.
use crate::{
    config::{duration, Limits},
    document::{ByteRange, SyntaxMatch},
    model::SyntaxSummary,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    time::Instant,
};
thread_local! {static CACHE:std::cell::RefCell<Option<PathBuf>>=const{std::cell::RefCell::new(None)};}
pub fn set_cache(path: Option<PathBuf>) {
    CACHE.with(|s| *s.borrow_mut() = path);
}
#[derive(Serialize, Deserialize)]
struct Node {
    kind: String,
    parent: Option<usize>,
    range: ByteRange,
    hash: String,
    leaf: bool,
    named: bool,
}
#[derive(Serialize, Deserialize)]
struct Parsed {
    source: String,
    nodes: Vec<Node>,
    shape: Vec<(u16, usize)>,
}
fn parse(path: &str, bytes: &[u8], limits: &Limits, start: Instant) -> Result<Parsed, String> {
    let (language, name) = crate::diff::grammar(path).ok_or("unsupported_language")?;
    let timeout = duration(&limits.syntax_timeout).map_err(|_| "syntax_budget_exceeded")?;
    let key = crate::hash(
        serde_json::to_vec(&(
            "syntax-descriptors-v1",
            name,
            include_str!("../Cargo.lock"),
            crate::hash(bytes),
            limits.max_syntax_nodes,
            &limits.syntax_timeout,
        ))
        .unwrap(),
    );
    let directory = CACHE.with(|s| s.borrow().clone());
    if let Some(dir) = &directory {
        if let Some(data) = crate::cache::get(dir, &key, limits.max_job_output_bytes) {
            if let Ok(parsed) = serde_json::from_slice::<Parsed>(&data) {
                if parsed.source == crate::hash(bytes)
                    && parsed.nodes.len() <= limits.max_syntax_nodes
                    && parsed.nodes.iter().enumerate().all(|(i, n)| {
                        n.range.start <= n.range.end
                            && n.range.end <= bytes.len()
                            && n.parent.is_none_or(|p| p < i)
                    })
                {
                    return Ok(parsed);
                }
            }
        }
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|_| "grammar_error")?;
    #[allow(deprecated)]
    parser.set_timeout_micros(
        timeout
            .saturating_sub(start.elapsed())
            .as_micros()
            .max(1)
            .min(u64::MAX as u128) as u64,
    );
    let tree = parser.parse(bytes, None).ok_or("syntax_budget_exceeded")?;
    if tree.root_node().has_error() {
        return Err("parse_error".into());
    }
    let mut result = Parsed {
        source: crate::hash(bytes),
        nodes: vec![],
        shape: vec![],
    };
    let mut stack = vec![(tree.root_node(), None)];
    let mut count = 0;
    while let Some((node, parent)) = stack.pop() {
        count += 1;
        if count > limits.max_syntax_nodes || start.elapsed() > timeout {
            return Err("syntax_budget_exceeded".into());
        }
        if node.kind().contains("comment") {
            continue;
        }
        let children: Vec<_> = (0..node.child_count())
            .filter_map(|i| node.child(i))
            .filter(|n| !n.kind().contains("comment"))
            .collect();
        result.shape.push((node.kind_id(), children.len()));
        let index = result.nodes.len();
        result.nodes.push(Node {
            kind: node.kind().into(),
            parent,
            range: ByteRange {
                start: node.start_byte(),
                end: node.end_byte(),
            },
            hash: crate::hash(&bytes[node.byte_range()]),
            leaf: children.is_empty(),
            named: node.is_named(),
        });
        for child in children.into_iter().rev() {
            stack.push((child, Some(index)));
        }
    }
    if let Some(dir) = directory {
        let _ = crate::cache::put(&dir, &key, &serde_json::to_vec(&result).unwrap());
    }
    Ok(result)
}
fn within(a: ByteRange, b: ByteRange) -> bool {
    a.start >= b.start && a.end <= b.end
}
pub fn compare(
    path: &str,
    old: &[u8],
    new: &[u8],
    limits: &Limits,
) -> Result<(SyntaxSummary, String, Vec<SyntaxMatch>), String> {
    let (_, name) = crate::diff::grammar(path).ok_or("unsupported_language")?;
    let (a_text, b_text) = (
        std::str::from_utf8(old).map_err(|_| "invalid_utf8")?,
        std::str::from_utf8(new).map_err(|_| "invalid_utf8")?,
    );
    let start = Instant::now();
    let timeout = duration(&limits.syntax_timeout).map_err(|_| "syntax_budget_exceeded")?;
    let (a, b) = (
        parse(path, old, limits, start)?,
        parse(path, new, limits, start)?,
    );
    let mut buckets: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, n) in b
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.leaf && n.named && !n.range.is_empty())
    {
        buckets.entry((&n.kind, &n.hash)).or_default().push(i);
    }
    let mut order: Vec<_> = a
        .nodes
        .iter()
        .filter(|n| !n.leaf && n.named && !n.range.is_empty())
        .collect();
    order.sort_by_key(|n| (std::cmp::Reverse(n.range.len()), n.range.start));
    let mut used = BTreeSet::new();
    let mut matches: Vec<SyntaxMatch> = vec![];
    let mut candidates = 0usize;
    for node in order {
        if start.elapsed() > timeout {
            return Err("syntax_budget_exceeded".into());
        }
        if matches.iter().any(|m| within(node.range, m.old)) {
            continue;
        }
        if let Some(indices) = buckets.get(&(node.kind.as_str(), node.hash.as_str())) {
            for &i in indices {
                candidates += 1;
                if candidates > limits.max_syntax_candidates || start.elapsed() > timeout {
                    return Err("syntax_budget_exceeded".into());
                }
                let other = &b.nodes[i];
                if used.contains(&i) || matches.iter().any(|m| within(other.range, m.new)) {
                    continue;
                }
                if old[node.range.start..node.range.end] == new[other.range.start..other.range.end]
                {
                    used.insert(i);
                    matches.push(SyntaxMatch {
                        old: node.range,
                        new: other.range,
                        kind: format!("subtree:{}", node.kind),
                        confidence: "exact_verified_bytes".into(),
                    });
                    break;
                }
            }
        }
    }
    let children = |p: &Parsed| {
        let mut children = vec![vec![]; p.nodes.len()];
        for (i, n) in p.nodes.iter().enumerate() {
            if let Some(parent) = n.parent {
                children[parent].push(i);
            }
        }
        children
    };
    let (ac, bc) = (children(&a), children(&b));
    let mut pairs = VecDeque::new();
    if !a.nodes.is_empty() && !b.nodes.is_empty() && a.nodes[0].kind == b.nodes[0].kind {
        pairs.push_back((0, 0));
    }
    while let Some((ai, bi)) = pairs.pop_front() {
        let mut next = 0;
        for &child in &ac[ai] {
            while next < bc[bi].len() {
                candidates += 1;
                if candidates > limits.max_syntax_candidates || start.elapsed() > timeout {
                    return Err("syntax_budget_exceeded".into());
                }
                let other = bc[bi][next];
                next += 1;
                let (an, bn) = (&a.nodes[child], &b.nodes[other]);
                if an.kind != bn.kind {
                    continue;
                }
                if !an.leaf && an.named && an.hash != bn.hash {
                    matches.push(SyntaxMatch {
                        old: an.range,
                        new: bn.range,
                        kind: format!("node:{}", an.kind),
                        confidence: "matched_parent_and_order".into(),
                    });
                }
                pairs.push_back((child, other));
                break;
            }
        }
    }
    let (aa, bb): (Vec<_>, Vec<_>) = (
        a.nodes.iter().filter(|n| n.leaf).collect(),
        b.nodes.iter().filter(|n| n.leaf).collect(),
    );
    if aa.len().saturating_mul(bb.len()).saturating_add(candidates) > limits.max_syntax_candidates {
        return Err("syntax_budget_exceeded".into());
    }
    let tokens = |nodes: &Vec<&Node>, parsed: &Parsed| {
        nodes
            .iter()
            .map(|n| {
                format!(
                    "{}:{}:{}",
                    n.parent.map_or("", |i| parsed.nodes[i].kind.as_str()),
                    n.kind,
                    n.hash
                )
            })
            .collect::<Vec<_>>()
    };
    let (at, bt) = (tokens(&aa, &a), tokens(&bb, &b));
    let (mut deleted, mut inserted) = (0, 0);
    for op in similar::capture_diff_slices_deadline(
        similar::Algorithm::Myers,
        &at,
        &bt,
        Some(start + timeout),
    ) {
        let (tag, ar, br) = op.as_tag_tuple();
        if tag != similar::DiffTag::Equal {
            deleted += ar.len();
            inserted += br.len();
            continue;
        }
        for (ai, bi) in ar.zip(br) {
            let (an, bn) = (aa[ai], bb[bi]);
            if !matches.iter().any(|m| {
                m.kind.starts_with("subtree:")
                    && (within(an.range, m.old) || within(bn.range, m.new))
            }) {
                matches.push(SyntaxMatch {
                    old: an.range,
                    new: bn.range,
                    kind: format!(
                        "{}:{}",
                        if an.named { "leaf" } else { "delimiter" },
                        an.kind
                    ),
                    confidence: "same_parent_kind_and_token".into(),
                });
            }
        }
    }
    if start.elapsed() > timeout {
        return Err("syntax_budget_exceeded".into());
    }
    matches.sort_by_key(|m| (m.old.start, m.new.start));
    if matches.iter().any(|m| {
        m.old.end > old.len()
            || m.new.end > new.len()
            || !a_text.is_char_boundary(m.old.start)
            || !a_text.is_char_boundary(m.old.end)
            || !b_text.is_char_boundary(m.new.start)
            || !b_text.is_char_boundary(m.new.end)
    }) {
        return Err("scope_projection_failed".into());
    }
    Ok((
        SyntaxSummary {
            unchanged_subtrees: matches
                .iter()
                .filter(|m| m.kind.starts_with("subtree:"))
                .count(),
            deleted_tokens: deleted,
            inserted_tokens: inserted,
            text_only_change: at == bt && a.shape == b.shape && old != new,
        },
        name.into(),
        matches,
    ))
}
