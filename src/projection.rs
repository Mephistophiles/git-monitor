//! Typed byte coordinates for renderer projections over selected sources.
use crate::{
    document::ByteRange,
    model::{Row, Span},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineIndex {
    pub starts: Vec<usize>,
    pub length: usize,
}
impl LineIndex {
    pub fn new(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter_map(|(i, b)| (*b == b'\n').then_some(i + 1)),
        );
        Self {
            starts,
            length: bytes.len(),
        }
    }
    pub fn line(&self, bytes: &[u8], number: usize, start_line: usize) -> Result<ByteRange> {
        let index = number
            .checked_sub(start_line)
            .context("line precedes selected range")?;
        let start = *self
            .starts
            .get(index)
            .context("line exceeds selected range")?;
        ensure!(start < bytes.len(), "line beyond source");
        let mut end = self.starts.get(index + 1).copied().unwrap_or(self.length);
        if end > start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        Ok(ByteRange { start, end })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Side {
    Old,
    New,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByteSpan {
    pub side: Side,
    pub range: ByteRange,
    pub syntax: String,
    pub changed: bool,
    pub move_id: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinePair {
    pub kind: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub old: Option<ByteRange>,
    pub new: Option<ByteRange>,
    pub spans: Vec<ByteSpan>,
    pub move_id: Option<usize>,
    pub notice: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub header: String,
    pub old: Option<ByteRange>,
    pub new: Option<ByteRange>,
    pub lines: Vec<LinePair>,
}
fn extent(a: Option<ByteRange>, b: Option<ByteRange>) -> Option<ByteRange> {
    match (a, b) {
        (Some(a), Some(b)) => Some(ByteRange {
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        }),
        (a, b) => a.or(b),
    }
}
pub fn project(
    rows: &[Row],
    old: &[u8],
    new: &[u8],
    old_start: usize,
    new_start: usize,
) -> Result<Vec<Hunk>> {
    let (ai, bi) = (LineIndex::new(old), LineIndex::new(new));
    let mut hunks: Vec<Hunk> = vec![];
    for row in rows {
        if row.kind == "hunk" || hunks.is_empty() {
            hunks.push(Hunk {
                header: if row.kind == "hunk" {
                    row.left.iter().map(|s| s.text.as_str()).collect()
                } else {
                    String::new()
                },
                old: None,
                new: None,
                lines: vec![],
            });
        }
        if row.kind == "hunk" {
            continue;
        }
        let (ar, br) = (
            row.old.map(|n| ai.line(old, n, old_start)).transpose()?,
            row.new.map(|n| bi.line(new, n, new_start)).transpose()?,
        );
        let mut spans = vec![];
        for (bytes, range, source, side) in [
            (old, ar, &row.left, Side::Old),
            (new, br, &row.right, Side::New),
        ] {
            if let Some(range) = range {
                ensure!(
                    String::from_utf8_lossy(&bytes[range.start..range.end])
                        == source.iter().map(|s| s.text.as_str()).collect::<String>(),
                    "presentation outside source line"
                );
                if std::str::from_utf8(&bytes[range.start..range.end]).is_ok() {
                    let mut offset = range.start;
                    for span in source {
                        let end = offset + span.text.len();
                        spans.push(ByteSpan {
                            side: side.clone(),
                            range: ByteRange { start: offset, end },
                            syntax: span.syntax.clone(),
                            changed: span.changed,
                            move_id: row.moved,
                        });
                        offset = end;
                    }
                }
            }
        }
        let h = hunks.last_mut().unwrap();
        h.old = extent(h.old, ar);
        h.new = extent(h.new, br);
        h.lines.push(LinePair {
            kind: row.kind.clone(),
            old_line: row.old,
            new_line: row.new,
            old: ar,
            new: br,
            spans,
            move_id: row.moved,
            notice: if ar.is_none() && br.is_none() {
                row.left.iter().map(|s| s.text.as_str()).collect()
            } else {
                String::new()
            },
        });
    }
    Ok(hunks)
}
pub fn rows(hunks: &[Hunk], old: &[u8], new: &[u8]) -> Result<Vec<Row>> {
    let mut rows = vec![];
    for h in hunks {
        rows.push(Row {
            kind: "hunk".into(),
            old: None,
            new: None,
            left: vec![Span {
                text: h.header.clone(),
                changed: false,
                syntax: String::new(),
            }],
            right: vec![],
            moved: None,
        });
        for line in &h.lines {
            let mut sides = [vec![], vec![]];
            for (index, bytes, range) in [(0, old, line.old), (1, new, line.new)] {
                if let Some(range) = range {
                    ensure!(
                        range.start <= range.end && range.end <= bytes.len(),
                        "line escaped source"
                    );
                    let selected: Vec<_> = line
                        .spans
                        .iter()
                        .filter(|s| matches!((&s.side, index), (Side::Old, 0) | (Side::New, 1)))
                        .collect();
                    if selected.is_empty() {
                        sides[index].push(Span {
                            text: String::from_utf8_lossy(&bytes[range.start..range.end]).into(),
                            changed: false,
                            syntax: String::new(),
                        });
                    } else {
                        let mut offset = range.start;
                        for span in selected {
                            ensure!(
                                span.range.start == offset
                                    && span.range.start <= span.range.end
                                    && span.range.end <= range.end,
                                "span escaped source line"
                            );
                            sides[index].push(Span {
                                text: std::str::from_utf8(
                                    &bytes[span.range.start..span.range.end],
                                )?
                                .into(),
                                changed: span.changed,
                                syntax: span.syntax.clone(),
                            });
                            offset = span.range.end;
                        }
                        ensure!(offset == range.end, "incomplete line spans");
                    }
                }
            }
            if line.old.is_none() && line.new.is_none() {
                sides[0].push(Span {
                    text: line.notice.clone(),
                    changed: false,
                    syntax: String::new(),
                });
            }
            let [left, right] = sides;
            rows.push(Row {
                kind: line.kind.clone(),
                old: line.old_line,
                new: line.new_line,
                left,
                right,
                moved: line.move_id,
            });
        }
    }
    Ok(rows)
}
pub fn validate(
    hunks: &[Hunk],
    old: &[u8],
    new: &[u8],
    old_start: usize,
    new_start: usize,
) -> Result<()> {
    let (ai, bi) = (LineIndex::new(old), LineIndex::new(new));
    for h in hunks {
        let (mut ar, mut br) = (None, None);
        for line in &h.lines {
            for span in &line.spans {
                let range = match span.side {
                    Side::Old => line.old,
                    Side::New => line.new,
                }
                .context("span on absent source side")?;
                ensure!(
                    span.range.start >= range.start
                        && span.range.start <= span.range.end
                        && span.range.end <= range.end,
                    "span outside line"
                );
            }
            ensure!(
                line.old
                    == line
                        .old_line
                        .map(|n| ai.line(old, n, old_start))
                        .transpose()?
                    && line.new
                        == line
                            .new_line
                            .map(|n| bi.line(new, n, new_start))
                            .transpose()?,
                "byte/line coordinate mismatch"
            );
            ar = extent(ar, line.old);
            br = extent(br, line.new);
        }
        ensure!(ar == h.old && br == h.new, "hunk extent mismatch");
    }
    rows(hunks, old, new)?;
    Ok(())
}
