//! Versioned, byte-oriented diff contract. Presentation is a projection of this
//! document; neither HTML nor terminal widths participate in edit coordinates.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}
impl ByteRange {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSlice {
    pub path: String,
    pub blob: Option<String>,
    pub mode: String,
    pub original_range: ByteRange,
    pub start_line: usize,
    pub sha256: String,
    pub valid_utf8: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Equal,
    Delete,
    Insert,
    Replace,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    pub kind: ChangeKind,
    pub old: ByteRange,
    pub new: ByteRange,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntaxMatch {
    pub old: ByteRange,
    pub new: ByteRange,
    pub kind: String,
    pub confidence: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffDocument {
    pub schema: u32,
    pub profile: String,
    pub old: SourceSlice,
    pub new: SourceSlice,
    pub edits: Vec<TextEdit>,
    pub syntax_matches: Vec<SyntaxMatch>,
    pub quality: Vec<String>,
}
fn offsets(bytes: &[u8]) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    if v.last() != Some(&bytes.len()) {
        v.push(bytes.len());
    }
    v
}
impl DiffDocument {
    pub fn build(
        path: &str,
        old: &[u8],
        new: &[u8],
        old_line: usize,
        new_line: usize,
        limit: usize,
    ) -> Self {
        let source = |bytes: &[u8], line| SourceSlice {
            path: path.into(),
            blob: None,
            mode: String::new(),
            original_range: ByteRange {
                start: 0,
                end: bytes.len(),
            },
            start_line: line,
            sha256: crate::hash(bytes),
            valid_utf8: std::str::from_utf8(bytes).is_ok(),
        };
        let (a, b) = (offsets(old), offsets(new));
        let (aa, bb): (Vec<_>, Vec<_>) = (
            a.windows(2).map(|r| &old[r[0]..r[1]]).collect(),
            b.windows(2).map(|r| &new[r[0]..r[1]]).collect(),
        );
        let mut quality = vec![];
        let edits = if aa.len().saturating_mul(bb.len()) > limit {
            quality.push("byte_edit_refinement_budget_exceeded".into());
            vec![TextEdit {
                kind: if old == new {
                    ChangeKind::Equal
                } else {
                    ChangeKind::Replace
                },
                old: ByteRange {
                    start: 0,
                    end: old.len(),
                },
                new: ByteRange {
                    start: 0,
                    end: new.len(),
                },
            }]
        } else {
            similar::capture_diff_slices_deadline(
                similar::Algorithm::Myers,
                &aa,
                &bb,
                Some(std::time::Instant::now() + std::time::Duration::from_secs(2)),
            )
            .into_iter()
            .map(|op| {
                let (kind, x, y) = op.as_tag_tuple();
                TextEdit {
                    kind: match kind {
                        similar::DiffTag::Equal => ChangeKind::Equal,
                        similar::DiffTag::Delete => ChangeKind::Delete,
                        similar::DiffTag::Insert => ChangeKind::Insert,
                        similar::DiffTag::Replace => ChangeKind::Replace,
                    },
                    old: ByteRange {
                        start: a[x.start],
                        end: a[x.end],
                    },
                    new: ByteRange {
                        start: b[y.start],
                        end: b[y.end],
                    },
                }
            })
            .collect()
        };
        Self {
            schema: crate::profile::MODEL_VERSION,
            profile: "byte-reconstruction-myers-v1".into(),
            old: source(old, old_line),
            new: source(new, new_line),
            edits,
            syntax_matches: vec![],
            quality,
        }
    }
    pub fn reconstruct(&self, old: &[u8], new: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            self.schema == crate::profile::MODEL_VERSION,
            "unsupported diff document schema"
        );
        ensure!(
            self.old.original_range.start <= self.old.original_range.end
                && self.new.original_range.start <= self.new.original_range.end,
            "invalid source interval"
        );
        ensure!(
            self.old.original_range.len() == old.len()
                && self.new.original_range.len() == new.len(),
            "source slice size mismatch"
        );
        ensure!(
            crate::hash(old) == self.old.sha256 && crate::hash(new) == self.new.sha256,
            "source slice checksum mismatch"
        );
        let (mut a, mut b) = (0, 0);
        let mut output = vec![];
        for e in &self.edits {
            ensure!(
                e.old.start == a
                    && e.new.start == b
                    && e.old.end >= a
                    && e.new.end >= b
                    && e.old.end <= old.len()
                    && e.new.end <= new.len(),
                "edit escaped selected source or overlapped"
            );
            match e.kind {
                ChangeKind::Equal => {
                    ensure!(old[a..e.old.end] == new[b..e.new.end], "invalid equal edit");
                    output.extend_from_slice(&old[a..e.old.end]);
                }
                ChangeKind::Insert => {
                    ensure!(e.old.is_empty(), "insert consumed old source");
                    output.extend_from_slice(&new[b..e.new.end]);
                }
                ChangeKind::Delete => ensure!(e.new.is_empty(), "delete produced new source"),
                ChangeKind::Replace => output.extend_from_slice(&new[b..e.new.end]),
            }
            a = e.old.end;
            b = e.new.end;
        }
        ensure!(
            a == old.len() && b == new.len() && output == new,
            "edits do not reconstruct complete allowed content"
        );
        for m in &self.syntax_matches {
            ensure!(
                m.old.start <= m.old.end
                    && m.old.end <= old.len()
                    && m.new.start <= m.new.end
                    && m.new.end <= new.len(),
                "syntax match escaped selected range"
            );
        }
        Ok(output)
    }
}
